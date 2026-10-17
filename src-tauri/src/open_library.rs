use std::{
    future::Future,
    time::{Duration, Instant},
};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

use crate::{
    app_error, clean_optional, clean_string_list, cleanup_cover_if_unreferenced,
    current_library_dir, ensure_library_open, get_book, now_string, persist_book_update,
    public_socket_for, validate_book_fields, AppState, Book, OperationStatus,
};

const OPEN_LIBRARY_ORIGIN: &str = "openlibrary.org";
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone)]
pub(crate) struct MetadataLookupSession {
    book_id: String,
    book_updated_at: String,
    candidates: Vec<MetadataCandidate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataLookup {
    token: String,
    current: Book,
    query_fields: Vec<String>,
    candidates: Vec<MetadataCandidate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetadataCandidate {
    id: String,
    title: Option<String>,
    authors: Option<Vec<String>>,
    isbn: Option<String>,
    publisher: Option<String>,
    publication_year: Option<i32>,
    genre: Option<String>,
    tags: Option<Vec<String>>,
    identifiers: Option<Vec<String>>,
    language: Option<String>,
    pages: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OpenLibraryResponse {
    #[serde(default)]
    docs: Vec<OpenLibraryDocument>,
}

#[derive(Debug, Deserialize)]
struct OpenLibraryDocument {
    key: String,
    title: Option<String>,
    author_name: Option<Vec<String>>,
    publisher: Option<Vec<String>>,
    first_publish_year: Option<i32>,
    #[serde(default)]
    isbn: Vec<String>,
    subject: Option<Vec<String>>,
    language: Option<Vec<String>>,
    number_of_pages_median: Option<u64>,
}

fn update_operation(
    state: &AppState,
    operation_id: Option<&str>,
    phase: &str,
    completed: usize,
    total: usize,
    cancellable: bool,
) -> Result<(), String> {
    if let Some(operation_id) = operation_id {
        state.operation_status.lock().map_err(app_error)?.insert(
            operation_id.to_owned(),
            OperationStatus {
                phase: phase.to_owned(),
                completed,
                total,
                cancellable,
            },
        );
    }
    Ok(())
}

fn check_cancelled(state: &AppState, operation_id: Option<&str>) -> Result<(), String> {
    if operation_id.is_some_and(|id| {
        state
            .cancelled_operations
            .lock()
            .map(|cancelled| cancelled.contains(id))
            .unwrap_or(false)
    }) {
        Err("OPERATION_CANCELLED: Metadata lookup was cancelled; no changes were saved.".into())
    } else {
        Ok(())
    }
}

async fn cancellable_request<T, F>(
    state: &AppState,
    operation_id: Option<&str>,
    request: F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, reqwest::Error>>,
{
    tokio::pin!(request);
    loop {
        check_cancelled(state, operation_id)?;
        tokio::select! {
            result = &mut request => {
                return result.map_err(|error| {
                    if error.is_timeout() {
                        "METADATA_TIMEOUT: Open Library lookup exceeded 15 seconds.".to_owned()
                    } else {
                        format!("METADATA_OFFLINE: Open Library request failed: {error}")
                    }
                });
            }
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

fn lookup_query(book: &Book) -> Result<(Vec<(String, String)>, Vec<String>), String> {
    if let Some(isbn) = book
        .isbn
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return Ok((
            vec![("isbn".into(), isbn.trim().into())],
            vec!["isbn".into()],
        ));
    }
    let mut terms = Vec::new();
    let mut fields = Vec::new();
    if !book.title.trim().is_empty() {
        terms.push(("title".into(), book.title.trim().into()));
        fields.push("title".into());
    }
    if !book.authors.is_empty() {
        terms.push(("author".into(), book.authors.join(" ")));
        fields.push("authors".into());
    }
    if terms.is_empty() {
        return Err(
            "METADATA_QUERY_EMPTY: Add a title, author, or ISBN before looking up metadata.".into(),
        );
    }
    Ok((terms, fields))
}

async fn download_response(
    state: &AppState,
    mut url: Url,
    operation_id: Option<&str>,
) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    for redirects in 0..=MAX_REDIRECTS {
        check_cancelled(state, operation_id)?;
        if url.scheme() != "https"
            || url.host_str() != Some(OPEN_LIBRARY_ORIGIN)
            || url.port_or_known_default() != Some(443)
        {
            return Err(
                "METADATA_ORIGIN_BLOCKED: Open Library redirects must remain on the approved HTTPS origin."
                    .into(),
            );
        }
        let remaining = LOOKUP_TIMEOUT
            .checked_sub(started.elapsed())
            .ok_or_else(|| {
                "METADATA_TIMEOUT: Open Library lookup exceeded 15 seconds.".to_owned()
            })?;
        let (host, address) = public_socket_for(&url)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(remaining)
            .no_proxy()
            .resolve(&host, address)
            .build()
            .map_err(app_error)?;
        let mut response =
            cancellable_request(state, operation_id, client.get(url.clone()).send()).await?;
        if response.status().is_redirection() {
            if redirects == MAX_REDIRECTS {
                return Err("METADATA_REDIRECTS: Open Library exceeded five redirects.".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| "METADATA_REDIRECTS: Redirect has no location.".to_owned())?
                .to_str()
                .map_err(|_| "METADATA_REDIRECTS: Redirect location is invalid.".to_owned())?;
            url = url
                .join(location)
                .map_err(|error| format!("METADATA_REDIRECTS: Invalid redirect: {error}"))?;
            continue;
        }
        if response.status().as_u16() == 429 {
            return Err("METADATA_QUOTA: Open Library request rate limit was exceeded.".into());
        }
        if !response.status().is_success() {
            return Err(format!(
                "METADATA_STATUS: Open Library returned HTTP {}.",
                response.status()
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err("METADATA_TOO_LARGE: Open Library response exceeded 5 MiB.".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = cancellable_request(state, operation_id, response.chunk()).await? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err("METADATA_TOO_LARGE: Open Library response exceeded 5 MiB.".into());
            }
            bytes.extend_from_slice(&chunk);
            update_operation(
                state,
                operation_id,
                "downloading-metadata",
                bytes.len(),
                response.content_length().unwrap_or(0) as usize,
                true,
            )?;
        }
        return Ok(bytes);
    }
    Err("METADATA_REDIRECTS: Open Library exceeded five redirects.".into())
}

fn candidate_from_document(document: OpenLibraryDocument) -> MetadataCandidate {
    let isbn = document
        .isbn
        .iter()
        .find(|identifier| identifier.chars().filter(char::is_ascii_digit).count() == 13)
        .or_else(|| {
            document.isbn.iter().find(|identifier| {
                identifier
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .count()
                    == 10
            })
        })
        .or_else(|| document.isbn.first())
        .cloned();
    let identifiers = clean_string_list(
        document
            .isbn
            .iter()
            .filter(|identifier| Some(identifier.as_str()) != isbn.as_deref()),
    );
    let tags = clean_string_list(document.subject.unwrap_or_default());
    MetadataCandidate {
        id: document.key,
        title: clean_optional(document.title),
        authors: document.author_name.map(clean_string_list),
        isbn,
        publisher: document
            .publisher
            .and_then(|values| clean_string_list(values).into_iter().next()),
        publication_year: document.first_publish_year,
        genre: tags.first().cloned(),
        tags: (!tags.is_empty()).then_some(tags),
        identifiers: (!identifiers.is_empty()).then_some(identifiers),
        language: document
            .language
            .and_then(|values| clean_string_list(values).into_iter().next()),
        pages: document.number_of_pages_median.filter(|pages| *pages > 0),
    }
}

#[tauri::command]
pub(crate) async fn lookup_book_metadata(
    state: State<'_, AppState>,
    book_id: String,
    operation_id: Option<String>,
) -> Result<MetadataLookup, String> {
    ensure_library_open(&state)?;
    let book = {
        let connection = state.connection.lock().map_err(app_error)?;
        get_book(&connection, &book_id)?
    };
    let (query, query_fields) = lookup_query(&book)?;
    update_operation(
        &state,
        operation_id.as_deref(),
        "requesting-metadata",
        0,
        0,
        true,
    )?;
    let mut url = Url::parse("https://openlibrary.org/search.json").map_err(app_error)?;
    {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in query {
            pairs.append_pair(&name, &value);
        }
        pairs
            .append_pair("limit", "10")
            .append_pair(
                "fields",
                "key,title,author_name,isbn,publisher,first_publish_year,subject,language,number_of_pages_median",
            );
    }
    let bytes = download_response(&state, url, operation_id.as_deref()).await?;
    let response: OpenLibraryResponse = serde_json::from_slice(&bytes).map_err(|error| {
        format!("METADATA_MALFORMED: Open Library response is invalid: {error}")
    })?;
    let candidates: Vec<MetadataCandidate> = response
        .docs
        .into_iter()
        .take(10)
        .map(candidate_from_document)
        .collect();
    if candidates.is_empty() {
        return Err("METADATA_NO_RESULTS: Open Library returned no matching books.".into());
    }
    let token = Uuid::new_v4().to_string();
    state.metadata_lookups.lock().map_err(app_error)?.insert(
        token.clone(),
        MetadataLookupSession {
            book_id: book.id.clone(),
            book_updated_at: book.updated_at.clone(),
            candidates: candidates.clone(),
        },
    );
    update_operation(
        &state,
        operation_id.as_deref(),
        "completed",
        candidates.len(),
        candidates.len(),
        false,
    )?;
    Ok(MetadataLookup {
        token,
        current: book,
        query_fields,
        candidates,
    })
}

#[tauri::command]
pub(crate) fn apply_book_metadata(
    state: State<'_, AppState>,
    token: String,
    candidate_id: String,
    approved_fields: Vec<String>,
) -> Result<Book, String> {
    ensure_library_open(&state)?;
    let session = state
        .metadata_lookups
        .lock()
        .map_err(app_error)?
        .get(&token)
        .cloned()
        .ok_or_else(|| {
            "METADATA_EXPIRED: Metadata review expired; run the lookup again.".to_owned()
        })?;
    let candidate = session
        .candidates
        .iter()
        .find(|candidate| candidate.id == candidate_id)
        .cloned()
        .ok_or_else(|| "METADATA_CANDIDATE: Select a valid metadata candidate.".to_owned())?;
    let allowed = [
        "title",
        "authors",
        "isbn",
        "publisher",
        "publicationYear",
        "genre",
        "tags",
        "identifiers",
        "language",
        "pages",
    ];
    if approved_fields
        .iter()
        .any(|field| !allowed.contains(&field.as_str()))
    {
        return Err("METADATA_FIELD: Metadata approval contains an unsupported field.".into());
    }
    let library_dir = current_library_dir(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let mut book = get_book(&connection, &session.book_id)?;
    if book.updated_at != session.book_updated_at {
        return Err(
            "METADATA_STALE: The book changed after lookup; review fresh metadata before saving."
                .into(),
        );
    }
    if approved_fields.is_empty() {
        drop(connection);
        state
            .metadata_lookups
            .lock()
            .map_err(app_error)?
            .remove(&token);
        return Ok(book);
    }
    let mut applied = false;
    for field in approved_fields {
        match field.as_str() {
            "title" => {
                if let Some(value) = candidate.title.clone() {
                    book.title = value;
                    applied = true;
                }
            }
            "authors" => {
                if let Some(value) = candidate.authors.clone() {
                    book.authors = clean_string_list(value);
                    applied = true;
                }
            }
            "isbn" => {
                if let Some(value) = candidate.isbn.clone() {
                    book.isbn = Some(value);
                    applied = true;
                }
            }
            "publisher" => {
                if let Some(value) = candidate.publisher.clone() {
                    book.publisher = Some(value);
                    applied = true;
                }
            }
            "publicationYear" => {
                if let Some(value) = candidate.publication_year {
                    book.publication_year = Some(value);
                    applied = true;
                }
            }
            "genre" => {
                if let Some(value) = candidate.genre.clone() {
                    book.genre = Some(value);
                    applied = true;
                }
            }
            "tags" => {
                if let Some(value) = candidate.tags.clone() {
                    book.tags = clean_string_list(value);
                    applied = true;
                }
            }
            "identifiers" => {
                if let Some(value) = candidate.identifiers.clone() {
                    book.identifiers = clean_string_list(value);
                    applied = true;
                }
            }
            "language" => {
                if let Some(value) = candidate.language.clone() {
                    book.language = Some(value);
                    applied = true;
                }
            }
            "pages" => {
                if let Some(value) = candidate.pages {
                    book.pages = Some(value);
                    applied = true;
                }
            }
            _ => unreachable!(),
        }
    }
    if !applied {
        drop(connection);
        state
            .metadata_lookups
            .lock()
            .map_err(app_error)?
            .remove(&token);
        return Ok(book);
    }
    validate_book_fields(&book.title, book.publication_year, book.pages)?;
    let previous_cover = book.cover_image.clone();
    book.updated_at = now_string();
    let (book, cleanup) =
        persist_book_update(&connection, &library_dir, previous_cover.as_ref(), book)?;
    drop(connection);
    if let Some(path) = cleanup {
        cleanup_cover_if_unreferenced(&state, &library_dir, &path)?;
    }
    state
        .metadata_lookups
        .lock()
        .map_err(app_error)?
        .remove(&token);
    Ok(book)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_open_library_fields_without_clearing_absent_values() {
        let candidate = candidate_from_document(OpenLibraryDocument {
            key: "/works/OL1W".into(),
            title: Some("  Rust Book  ".into()),
            author_name: Some(vec![" Ferris ".into(), "".into()]),
            publisher: Some(vec![" Press ".into()]),
            first_publish_year: Some(2026),
            isbn: vec!["0012345678".into(), "9780012345678".into()],
            subject: Some(vec!["Computers".into(), "Programming".into()]),
            language: Some(vec!["eng".into()]),
            number_of_pages_median: Some(320),
        });
        assert_eq!(candidate.title.as_deref(), Some("Rust Book"));
        assert_eq!(candidate.authors, Some(vec!["Ferris".into()]));
        assert_eq!(candidate.isbn.as_deref(), Some("9780012345678"));
        assert_eq!(candidate.identifiers, Some(vec!["0012345678".into()]));
        assert_eq!(candidate.publication_year, Some(2026));
        assert_eq!(candidate.genre.as_deref(), Some("Computers"));
        assert_eq!(
            candidate.tags,
            Some(vec!["Computers".into(), "Programming".into()])
        );
        assert_eq!(candidate.pages, Some(320));
    }

    #[test]
    fn query_prefers_isbn_then_current_title_and_authors() {
        let now = now_string();
        let mut book = Book {
            schema_version: 2,
            id: "book".into(),
            title: "A Title".into(),
            isbn: Some("000-123".into()),
            authors: vec!["An Author".into()],
            publisher: None,
            publication_year: None,
            genre: None,
            ddc: None,
            lcc: None,
            tags: vec![],
            identifiers: vec![],
            language: None,
            pages: None,
            notes: None,
            cover_image: None,
            created_at: now.clone(),
            updated_at: now,
            extra: Default::default(),
        };
        assert_eq!(
            lookup_query(&book).unwrap().0,
            vec![("isbn".into(), "000-123".into())]
        );
        book.isbn = None;
        assert_eq!(
            lookup_query(&book).unwrap().0,
            vec![
                ("title".into(), "A Title".into()),
                ("author".into(), "An Author".into())
            ]
        );
    }
}
