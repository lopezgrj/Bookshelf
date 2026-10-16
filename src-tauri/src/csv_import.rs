use std::{collections::HashSet, fs};

use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::{
    app_error, clean_optional, clean_string_list, current_library_dir, ensure_library_open,
    identity_key, list_documents, now_string, save_document, validate_book_fields, AppState, Book,
    OperationStatus, MAX_CSV_BYTES, SCHEMA_VERSION,
};
use std::collections::HashMap;

const COLUMNS: [&str; 13] = [
    "title",
    "isbn",
    "authors",
    "publisher",
    "publication_year",
    "genre",
    "ddc",
    "lcc",
    "tags",
    "identifiers",
    "language",
    "pages",
    "notes",
];
const MAX_RECORDS: usize = 10_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportIssue {
    record: u64,
    column: String,
    code: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportRow {
    record: u64,
    book: Book,
    duplicate: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportPreview {
    token: String,
    rows: Vec<ImportRow>,
    errors: Vec<ImportIssue>,
    unknown_columns: Vec<String>,
    delimiter: Option<char>,
    library_path: String,
}

fn issue(record: u64, column: &str, code: &str) -> ImportIssue {
    ImportIssue {
        record,
        column: column.into(),
        code: code.into(),
    }
}

// The CSV crate intentionally tolerates stray/unclosed quotes. Reject those before parsing.
fn check_quotes(text: &str, delimiter: u8) -> Result<(), ImportIssue> {
    #[derive(Clone, Copy)]
    enum Position {
        Start,
        Unquoted,
        Quoted,
        AfterQuote,
    }
    let mut position = Position::Start;
    let mut record = 1;
    let mut column = 1;
    let mut previous_cr = false;
    for byte in text.bytes() {
        if byte == b'\n' && previous_cr {
            previous_cr = false;
            continue;
        }
        previous_cr = false;
        position = match (position, byte) {
            (Position::Start, b'"') => Position::Quoted,
            (Position::Quoted, b'"') => Position::AfterQuote,
            (Position::Quoted, _) => continue,
            (Position::AfterQuote, b'"') => Position::Quoted,
            (Position::AfterQuote, b'\n' | b'\r')
            | (Position::Start | Position::Unquoted, b'\n' | b'\r') => {
                record += 1;
                column = 1;
                previous_cr = byte == b'\r';
                Position::Start
            }
            (Position::AfterQuote, _) if byte == delimiter => {
                column += 1;
                Position::Start
            }
            (Position::Start | Position::Unquoted, _) if byte == delimiter => {
                column += 1;
                Position::Start
            }
            _ => match position {
                Position::AfterQuote => {
                    return Err(issue(record, &column.to_string(), "malformed_csv"));
                }
                Position::Unquoted if byte == b'"' => {
                    return Err(issue(record, &column.to_string(), "malformed_csv"));
                }
                _ => Position::Unquoted,
            },
        };
    }
    if matches!(position, Position::Quoted) {
        return Err(issue(record, &column.to_string(), "malformed_csv"));
    }
    Ok(())
}

fn normalized_isbn(book: &Book) -> String {
    book.isbn
        .as_deref()
        .unwrap_or_default()
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .flat_map(char::to_uppercase)
        .collect()
}

fn title_authors_key(book: &Book) -> String {
    let mut authors: Vec<String> = book
        .authors
        .iter()
        .map(|author| identity_key(author.trim()))
        .collect();
    authors.sort();
    format!(
        "{}\u{1f}{}",
        identity_key(book.title.trim()),
        authors.join("\u{1e}")
    )
}

fn mark_duplicates(rows: &mut [ImportRow], existing: &[Book]) {
    let mut existing_isbns = HashSet::new();
    let mut existing_titles = HashSet::new();
    for book in existing {
        let isbn = normalized_isbn(book);
        if !isbn.is_empty() {
            existing_isbns.insert(isbn);
        }
        existing_titles.insert(title_authors_key(book));
    }
    let mut isbn_counts: HashMap<String, usize> = HashMap::new();
    let mut title_counts: HashMap<String, usize> = HashMap::new();
    for row in rows.iter() {
        let isbn = normalized_isbn(&row.book);
        if !isbn.is_empty() {
            *isbn_counts.entry(isbn).or_default() += 1;
        }
        *title_counts
            .entry(title_authors_key(&row.book))
            .or_default() += 1;
    }
    for row in rows {
        let isbn = normalized_isbn(&row.book);
        let title_key = title_authors_key(&row.book);
        row.duplicate = (!isbn.is_empty()
            && (existing_isbns.contains(&isbn)
                || isbn_counts.get(&isbn).copied().unwrap_or(0) > 1))
            || existing_titles.contains(&title_key)
            || title_counts.get(&title_key).copied().unwrap_or(0) > 1;
    }
}

fn header_for(text: &str, delimiter: u8) -> Result<Vec<String>, ()> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = reader.headers().map_err(|_| ())?;
    Ok(headers.iter().map(str::to_owned).collect())
}

fn valid_header(headers: &[String]) -> bool {
    let unique: HashSet<&str> = headers.iter().map(String::as_str).collect();
    unique.len() == headers.len() && unique.contains("title")
}

#[cfg(test)]
fn parse_csv_delimited(
    bytes: &[u8],
    existing: &[Book],
    requested_delimiter: Option<char>,
    library_path: String,
) -> ImportPreview {
    parse_csv_delimited_with_cancel(bytes, existing, requested_delimiter, library_path, || false)
}

fn parse_csv_delimited_with_cancel(
    bytes: &[u8],
    existing: &[Book],
    requested_delimiter: Option<char>,
    library_path: String,
    mut cancelled: impl FnMut() -> bool,
) -> ImportPreview {
    let mut preview = ImportPreview {
        token: Uuid::new_v4().to_string(),
        rows: Vec::new(),
        errors: Vec::new(),
        unknown_columns: Vec::new(),
        delimiter: None,
        library_path,
    };
    if bytes.len() > MAX_CSV_BYTES {
        preview.errors.push(issue(1, "", "file_too_large"));
        return preview;
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.trim_start_matches('\u{feff}'),
        Err(_) => {
            preview.errors.push(issue(1, "", "invalid_encoding"));
            return preview;
        }
    };
    let delimiter = match requested_delimiter {
        Some(',') => b',',
        Some('|') => b'|',
        Some(_) => {
            preview.errors.push(issue(1, "", "invalid_delimiter"));
            return preview;
        }
        None => {
            let comma = header_for(text, b',')
                .ok()
                .is_some_and(|headers| valid_header(&headers));
            let pipe = header_for(text, b'|')
                .ok()
                .is_some_and(|headers| valid_header(&headers));
            match (comma, pipe) {
                (true, false) => b',',
                (false, true) => b'|',
                _ => {
                    preview.errors.push(issue(1, "", "delimiter_required"));
                    return preview;
                }
            }
        }
    };
    preview.delimiter = Some(delimiter as char);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = match reader.headers() {
        Ok(headers) => headers.clone(),
        Err(_) => {
            preview.errors.push(issue(1, "", "malformed_csv"));
            return preview;
        }
    };
    if let Err(mut error) = check_quotes(text, delimiter) {
        if let Ok(index) = error.column.parse::<usize>() {
            if let Some(header) = headers.get(index - 1) {
                error.column = header.into();
            }
        }
        preview.errors.push(error);
        return preview;
    }
    let mut seen = HashSet::new();
    for header in &headers {
        if !seen.insert(header) {
            preview.errors.push(issue(1, header, "duplicate_header"));
        }
        if !COLUMNS.contains(&header) {
            preview.unknown_columns.push(header.into());
        }
    }
    if !seen.contains("title") {
        preview
            .errors
            .push(issue(1, "title", "missing_title_header"));
    }
    if !preview.errors.is_empty() {
        return preview;
    }
    let mut nonempty_records = 0usize;
    for (index, result) in reader.records().enumerate() {
        let record_number = index as u64 + 2;
        if cancelled() {
            preview.rows.clear();
            preview
                .errors
                .push(issue(record_number, "", "operation_cancelled"));
            return preview;
        }
        let record = match result {
            Ok(record) => record,
            Err(_) => {
                preview
                    .errors
                    .push(issue(record_number, "", "malformed_csv"));
                continue;
            }
        };
        if record.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        nonempty_records += 1;
        if nonempty_records > MAX_RECORDS {
            preview
                .errors
                .push(issue(record_number, "", "too_many_records"));
            break;
        }
        if record.len() != headers.len() {
            preview.errors.push(issue(record_number, "", "field_count"));
            continue;
        }
        let cell = |name: &str| {
            headers
                .iter()
                .position(|header| header == name)
                .and_then(|index| record.get(index))
                .unwrap_or_default()
                .trim()
        };
        let year_text = cell("publication_year");
        let year = if year_text.is_empty() {
            None
        } else {
            match year_text.parse::<i32>() {
                Ok(year) if year >= 0 => Some(year),
                _ => {
                    preview
                        .errors
                        .push(issue(record_number, "publication_year", "invalid_year"));
                    None
                }
            }
        };
        let pages_text = cell("pages");
        let pages = if pages_text.is_empty() {
            None
        } else {
            match pages_text.parse::<u64>() {
                Ok(pages) if pages > 0 => Some(pages),
                _ => {
                    preview
                        .errors
                        .push(issue(record_number, "pages", "invalid_pages"));
                    None
                }
            }
        };
        if let Err(error) = validate_book_fields(cell("title"), year, pages) {
            let (column, code) = if error.starts_with("REQUIRED_TITLE:") {
                ("title", "required_title")
            } else if error.starts_with("INVALID_PAGES:") {
                ("pages", "invalid_pages")
            } else {
                ("publication_year", "invalid_year")
            };
            preview.errors.push(issue(record_number, column, code));
        }
        let optional = |name| clean_optional(Some(cell(name).to_owned()));
        let now = now_string();
        preview.rows.push(ImportRow {
            record: record_number,
            book: Book {
                schema_version: SCHEMA_VERSION,
                id: Uuid::new_v4().to_string(),
                title: cell("title").into(),
                isbn: optional("isbn"),
                authors: cell("authors")
                    .split(';')
                    .filter_map(|name| clean_optional(Some(name.into())))
                    .collect(),
                publisher: optional("publisher"),
                publication_year: year,
                genre: optional("genre"),
                ddc: optional("ddc"),
                lcc: optional("lcc"),
                tags: clean_string_list(cell("tags").split(';')),
                identifiers: clean_string_list(cell("identifiers").split(';')),
                language: optional("language"),
                pages,
                notes: optional("notes"),
                cover_image: None,
                created_at: now.clone(),
                updated_at: now,
                extra: serde_json::Map::new(),
            },
            duplicate: false,
        });
    }
    mark_duplicates(&mut preview.rows, existing);
    preview
}

#[cfg(test)]
fn parse_csv(bytes: &[u8], existing: &[Book]) -> ImportPreview {
    parse_csv_delimited(bytes, existing, Some(','), String::new())
}

#[cfg(test)]
fn commit_preview(
    connection: &Connection,
    preview: &ImportPreview,
    allow_duplicates: bool,
    ignore_unknown_columns: bool,
) -> Result<usize, String> {
    commit_preview_with_operation(
        None,
        connection,
        preview,
        allow_duplicates,
        ignore_unknown_columns,
        None,
    )
}

fn commit_preview_with_operation(
    state: Option<&AppState>,
    connection: &Connection,
    preview: &ImportPreview,
    allow_duplicates: bool,
    ignore_unknown_columns: bool,
    operation_id: Option<&str>,
) -> Result<usize, String> {
    commit_preview_checked(
        state,
        connection,
        preview,
        allow_duplicates,
        ignore_unknown_columns,
        operation_id,
        || {
            operation_id.is_some_and(|id| {
                state.is_some_and(|state| {
                    state
                        .cancelled_operations
                        .lock()
                        .map(|set| set.contains(id))
                        .unwrap_or(false)
                })
            })
        },
    )
}

fn commit_preview_checked(
    state: Option<&AppState>,
    connection: &Connection,
    preview: &ImportPreview,
    allow_duplicates: bool,
    ignore_unknown_columns: bool,
    operation_id: Option<&str>,
    mut cancelled: impl FnMut() -> bool,
) -> Result<usize, String> {
    if !preview.errors.is_empty() || preview.rows.is_empty() {
        return Err("CSV_INVALID: Resolve validation errors before importing.".into());
    }
    if !preview.unknown_columns.is_empty() && !ignore_unknown_columns {
        return Err("CSV_UNKNOWN_COLUMNS: Confirm ignoring unknown columns.".into());
    }
    if let (Some(state), Some(operation_id)) = (state, operation_id) {
        let mut operations = state.operation_status.lock().map_err(app_error)?;
        let cancelled_operations = state.cancelled_operations.lock().map_err(app_error)?;
        if cancelled_operations.contains(operation_id) {
            return Err(
                "OPERATION_CANCELLED: Import was cancelled before commit; no books were added."
                    .into(),
            );
        }
        operations.insert(
            operation_id.into(),
            OperationStatus {
                phase: "committing".into(),
                completed: 0,
                total: preview.rows.len(),
                cancellable: false,
            },
        );
    } else if cancelled() {
        return Err(
            "OPERATION_CANCELLED: Import was cancelled before commit; no books were added.".into(),
        );
    }
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(app_error)?;
    let existing: Vec<Book> = list_documents(&tx, "books")?;
    let mut rows = preview.rows.clone();
    mark_duplicates(&mut rows, &existing);
    if !allow_duplicates && rows.iter().any(|row| row.duplicate) {
        return Err("CSV_DUPLICATES: Confirm importing duplicates as separate books.".into());
    }
    for row in &rows {
        validate_book_fields(&row.book.title, row.book.publication_year, row.book.pages)?;
        let mut book = row.book.clone();
        book.id = Uuid::new_v4().to_string();
        book.created_at = now_string();
        book.updated_at = book.created_at.clone();
        save_document(&tx, "books", &book.id, &book)?;
    }
    tx.commit().map_err(app_error)?;
    Ok(rows.len())
}

#[tauri::command(async)]
pub(crate) fn preview_book_csv(
    state: State<'_, AppState>,
    path: String,
    delimiter: Option<char>,
    operation_id: Option<String>,
) -> Result<ImportPreview, String> {
    ensure_library_open(&state)?;
    if let Some(operation_id) = operation_id.as_deref() {
        state.operation_status.lock().map_err(app_error)?.insert(
            operation_id.into(),
            OperationStatus {
                phase: "parsing".into(),
                completed: 0,
                total: 1,
                cancellable: true,
            },
        );
    }
    let metadata = fs::metadata(&path).map_err(app_error)?;
    if metadata.len() > MAX_CSV_BYTES as u64 {
        if let Some(operation_id) = operation_id.as_deref() {
            state.operation_status.lock().map_err(app_error)?.insert(
                operation_id.into(),
                OperationStatus {
                    phase: "failed".into(),
                    completed: 0,
                    total: 0,
                    cancellable: false,
                },
            );
        }
        return Err("CSV_TOO_LARGE: CSV files may not exceed 20 MiB.".into());
    }
    let bytes = fs::read(path).map_err(app_error)?;
    if operation_id.as_deref().is_some_and(|id| {
        state
            .cancelled_operations
            .lock()
            .map(|set| set.contains(id))
            .unwrap_or(false)
    }) {
        if let Some(operation_id) = operation_id.as_deref() {
            state.operation_status.lock().map_err(app_error)?.insert(
                operation_id.into(),
                OperationStatus {
                    phase: "cancelled".into(),
                    completed: 0,
                    total: 0,
                    cancellable: false,
                },
            );
        }
        return Err("OPERATION_CANCELLED: CSV preview was cancelled.".into());
    }
    let connection = state.connection.lock().map_err(app_error)?;
    let mut pending = state.csv_preview.lock().map_err(app_error)?;
    let existing = list_documents(&connection, "books")?;
    let library_path = current_library_dir(&state)?.display().to_string();
    let preview =
        parse_csv_delimited_with_cancel(&bytes, &existing, delimiter, library_path, || {
            operation_id.as_deref().is_some_and(|id| {
                state
                    .cancelled_operations
                    .lock()
                    .map(|set| set.contains(id))
                    .unwrap_or(false)
            })
        });
    if preview
        .errors
        .iter()
        .any(|error| error.code == "operation_cancelled")
    {
        *pending = None;
        if let Some(operation_id) = operation_id {
            state.operation_status.lock().map_err(app_error)?.insert(
                operation_id,
                OperationStatus {
                    phase: "cancelled".into(),
                    completed: 0,
                    total: 0,
                    cancellable: false,
                },
            );
        }
        return Err("OPERATION_CANCELLED: CSV preview was cancelled.".into());
    }
    *pending = Some(preview.clone());
    if let Some(operation_id) = operation_id {
        state.operation_status.lock().map_err(app_error)?.insert(
            operation_id,
            OperationStatus {
                phase: "completed".into(),
                completed: 1,
                total: 1,
                cancellable: false,
            },
        );
    }
    Ok(preview)
}

#[tauri::command(async)]
pub(crate) fn import_book_csv(
    state: State<'_, AppState>,
    token: String,
    allow_duplicates: bool,
    ignore_unknown_columns: bool,
    operation_id: Option<String>,
) -> Result<usize, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let mut pending = state.csv_preview.lock().map_err(app_error)?;
    let preview = pending
        .as_ref()
        .filter(|preview| {
            preview.token == token
                && preview.library_path
                    == current_library_dir(&state)
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
        })
        .ok_or_else(|| "CSV_EXPIRED: Select the CSV file again.".to_owned())?;
    let operation_id_ref = operation_id.as_deref();
    if let Some(operation_id) = operation_id_ref {
        state.operation_status.lock().map_err(app_error)?.insert(
            operation_id.into(),
            OperationStatus {
                phase: "importing".into(),
                completed: 0,
                total: preview.rows.len(),
                cancellable: true,
            },
        );
    }
    let result = commit_preview_with_operation(
        Some(&state),
        &connection,
        preview,
        allow_duplicates,
        ignore_unknown_columns,
        operation_id_ref,
    );
    match result {
        Ok(count) => {
            *pending = None;
            if let Some(operation_id) = operation_id {
                state.operation_status.lock().map_err(app_error)?.insert(
                    operation_id,
                    OperationStatus {
                        phase: "completed".into(),
                        completed: count,
                        total: count,
                        cancellable: false,
                    },
                );
            }
            Ok(count)
        }
        Err(error) => {
            if error.starts_with("OPERATION_CANCELLED:") {
                *pending = None;
            }
            if let Some(operation_id) = operation_id {
                state.operation_status.lock().map_err(app_error)?.insert(
                    operation_id,
                    OperationStatus {
                        phase: if error.starts_with("OPERATION_CANCELLED:") {
                            "cancelled"
                        } else {
                            "failed"
                        }
                        .into(),
                        completed: 0,
                        total: 0,
                        cancellable: false,
                    },
                );
            }
            Err(error)
        }
    }
}

#[tauri::command]
pub(crate) fn cancel_book_csv(state: State<'_, AppState>, token: String) -> Result<(), String> {
    let mut pending = state.csv_preview.lock().map_err(app_error)?;
    if pending
        .as_ref()
        .is_some_and(|preview| preview.token == token)
    {
        *pending = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{open_database, read_document};
    use tempfile::tempdir;

    #[test]
    fn cancellation_during_parsing_discards_partial_preview_without_writes() {
        let source = b"title\nFirst\nSecond\nThird\n";
        let mut checks = 0;
        let preview =
            parse_csv_delimited_with_cancel(source, &[], Some(','), "fixture".into(), || {
                checks += 1;
                checks == 2
            });
        assert_eq!(checks, 2);
        assert!(preview.rows.is_empty());
        assert_eq!(preview.errors[0].code, "operation_cancelled");
        let dir = tempfile::Builder::new()
            .prefix(".backend-csv-")
            .tempdir_in(".")
            .unwrap();
        let connection = open_database(&dir.path().join("library.sqlite3")).unwrap();
        assert!(commit_preview(&connection, &preview, true, true)
            .unwrap_err()
            .starts_with("CSV_INVALID:"));
        assert!(list_documents::<Book>(&connection, "books")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cancellation_is_checked_before_the_transaction_and_disabled_during_commit() {
        let dir = tempfile::Builder::new()
            .prefix(".backend-csv-")
            .tempdir_in(".")
            .unwrap();
        let connection = open_database(&dir.path().join("library.sqlite3")).unwrap();
        let source = b"title\nFirst\nSecond\nThird\n";
        let preview = parse_csv(source, &[]);
        let error = commit_preview_checked(None, &connection, &preview, true, true, None, || true)
            .unwrap_err();
        assert!(error.starts_with("OPERATION_CANCELLED:"));
        assert!(list_documents::<Book>(&connection, "books")
            .unwrap()
            .is_empty());

        let mut cancellation_requested = false;
        let mut checks = 0;
        let imported =
            commit_preview_checked(None, &connection, &preview, true, true, None, || {
                checks += 1;
                cancellation_requested = true;
                false
            })
            .unwrap();
        assert_eq!(imported, 3);
        assert_eq!(checks, 1);
        assert!(cancellation_requested);
        assert_eq!(
            list_documents::<Book>(&connection, "books").unwrap().len(),
            3
        );
    }

    #[test]
    fn production_import_marks_the_operation_non_cancellable_before_database_work() {
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = crate::tests::isolated_state(&root);
        let connection = open_database(&root.join("library.sqlite3")).unwrap();
        let preview = parse_csv(b"title\nFirst\n", &[]);
        state.operation_status.lock().unwrap().insert(
            "import".into(),
            OperationStatus {
                phase: "importing".into(),
                completed: 0,
                total: 1,
                cancellable: true,
            },
        );
        let mut cancellation_checks = 0;

        assert_eq!(
            commit_preview_checked(
                Some(&state),
                &connection,
                &preview,
                true,
                true,
                Some("import"),
                || {
                    cancellation_checks += 1;
                    true
                },
            )
            .unwrap(),
            1
        );
        assert_eq!(cancellation_checks, 0);
        let status = state
            .operation_status
            .lock()
            .unwrap()
            .get("import")
            .unwrap()
            .clone();
        assert_eq!(status.phase, "committing");
        assert!(!status.cancellable);
    }

    #[test]
    fn parses_bom_unicode_quotes_multiline_and_both_line_endings() {
        for newline in ["\n", "\r\n"] {
            let csv = format!(
                "\u{feff}title,isbn,authors,publisher,publication_year,genre,notes{newline}\
                 \"École, part \"\"one\"\"\",001234,María; François,Maison,2024,Essays,\"First{newline}Second\"{newline}\
                 ,,,,,,{newline}Untitled ISBN,,,,,,{newline}"
            );
            let preview = parse_csv(csv.as_bytes(), &[]);
            assert!(preview.errors.is_empty(), "{:?}", preview.errors);
            assert_eq!(preview.rows.len(), 2);
            let book = &preview.rows[0].book;
            assert_eq!(book.title, "École, part \"one\"");
            assert_eq!(book.isbn.as_deref(), Some("001234"));
            assert_eq!(book.authors, ["María", "François"]);
            assert_eq!(book.publication_year, Some(2024));
            assert_eq!(
                book.notes.as_deref(),
                Some(format!("First{newline}Second").as_str())
            );
            assert!(book.cover_image.is_none());
            assert_eq!(preview.rows[1].book.isbn, None);
            assert!(preview.rows[1].book.authors.is_empty());
        }
    }

    #[test]
    fn detects_pipe_delimiter_and_preserves_classification_values() {
        let source = b"title|authors|publication_year|ddc|lcc\n\"River | Bank\"|Maria;Ana|2024|005.133|QA76.73.R87";
        let detected = parse_csv_delimited(source, &[], None, "test-library".into());
        assert_eq!(detected.delimiter, Some('|'));
        assert!(detected.errors.is_empty(), "{:?}", detected.errors);
        assert_eq!(detected.rows[0].book.title, "River | Bank");
        assert_eq!(detected.rows[0].book.ddc.as_deref(), Some("005.133"));
        assert_eq!(detected.rows[0].book.lcc.as_deref(), Some("QA76.73.R87"));

        let ambiguous = parse_csv_delimited(b"title\nOnly title\n", &[], None, String::new());
        assert_eq!(ambiguous.errors[0].code, "delimiter_required");
        let explicit = parse_csv_delimited(b"title\nOnly title\n", &[], Some(','), String::new());
        assert!(explicit.errors.is_empty());
    }

    #[test]
    fn parses_new_book_fields_and_rejects_invalid_page_counts() {
        let source =
            "title,tags,identifiers,language,pages\nBook,\" Reference ; Rust \",00123;AB-7,Français,240\n";
        let preview = parse_csv(source.as_bytes(), &[]);
        assert!(preview.errors.is_empty(), "{:?}", preview.errors);
        let book = &preview.rows[0].book;
        assert_eq!(book.tags, ["Reference", "Rust"]);
        assert_eq!(book.identifiers, ["00123", "AB-7"]);
        assert_eq!(book.language.as_deref(), Some("Français"));
        assert_eq!(book.pages, Some(240));
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        assert_eq!(
            commit_preview(&connection, &preview, true, true).unwrap(),
            1
        );
        let saved = list_documents::<Book>(&connection, "books")
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(saved.tags, ["Reference", "Rust"]);
        assert_eq!(saved.identifiers, ["00123", "AB-7"]);
        assert_eq!(saved.language.as_deref(), Some("Français"));
        assert_eq!(saved.pages, Some(240));

        for value in ["0", "-1", "1.5", "18446744073709551616"] {
            let source = format!("title,pages\nBook,{value}\n");
            let invalid = parse_csv(source.as_bytes(), &[]);
            assert_eq!(invalid.errors.len(), 1, "{value}: {:?}", invalid.errors);
            assert_eq!(invalid.errors[0].column, "pages");
            assert_eq!(invalid.errors[0].code, "invalid_pages");
        }
    }

    #[test]
    fn csv_limits_are_rejected_before_import() {
        let large = vec![b'a'; MAX_CSV_BYTES + 1];
        assert_eq!(
            parse_csv_delimited(&large, &[], Some(','), String::new()).errors[0].code,
            "file_too_large"
        );
        let mut records = String::from("title\n");
        for index in 0..=MAX_RECORDS {
            records.push_str(&format!("Book {index}\n"));
        }
        let preview = parse_csv_delimited(records.as_bytes(), &[], Some(','), String::new());
        assert!(preview
            .errors
            .iter()
            .any(|issue| issue.code == "too_many_records"));
        assert!(preview.rows.len() <= MAX_RECORDS);
    }

    #[test]
    fn rejects_invalid_headers_encoding_quoting_and_field_counts() {
        let cases: &[(&[u8], &str)] = &[
            (b"", "missing_title_header"),
            (b"isbn\n123\n", "missing_title_header"),
            (b"title,title\nA,B\n", "duplicate_header"),
            (b"title\n\xff\n", "invalid_encoding"),
            (b"title\n\"Unclosed\n", "malformed_csv"),
            (b"title\nUn\"quoted\n", "malformed_csv"),
            (b"title\n\"Closed\"tail\n", "malformed_csv"),
            (b"title,isbn\nOnly title\n", "field_count"),
            (b"title\nToo,many\n", "field_count"),
        ];
        for (bytes, code) in cases {
            let preview = parse_csv(bytes, &[]);
            assert!(
                preview.errors.iter().any(|issue| issue.code == *code),
                "{preview:?}"
            );
        }
    }

    #[test]
    fn validation_reports_record_and_column_and_blocks_all_rows() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        let preview = parse_csv(
            b"title,publication_year\nGood,2024\n ,2020\nNegative,-1\nDecimal,1.2\nOverflow,999999999999\n",
            &[],
        );
        assert_eq!(preview.errors.len(), 4);
        assert_eq!(preview.errors[0].record, 3);
        assert_eq!(preview.errors[0].column, "title");
        assert_eq!(preview.errors[1].column, "publication_year");
        assert!(commit_preview(&connection, &preview, true, true).is_err());
        assert!(list_documents::<Book>(&connection, "books")
            .unwrap()
            .is_empty());
        assert!(validate_book_fields("Manual", Some(-1), None).is_err());
    }

    #[test]
    fn flags_normalized_isbn_and_case_insensitive_author_duplicates() {
        let original = parse_csv(b"title,isbn,authors\nOriginal,00-123-x,Alice;Bob\n", &[]);
        let preview = parse_csv(
            b"title,isbn,authors\nDifferent,00 123 X,Someone\nORIGINAL,,BOB;ALICE\nPair,,Reader\npair,,READER\n",
            &[original.rows[0].book.clone()],
        );
        assert!(preview.errors.is_empty());
        assert!(preview.rows.iter().all(|row| row.duplicate));
        let distinct = parse_csv(b"title,isbn,authors\nOther,,Other\n", &[]);
        assert!(!distinct.rows[0].duplicate);

        let whitespace_duplicates = parse_csv(
            "title,authors\n\"A   Shared Title\",Ana;Béa\na shared title,Béa;Ana\n".as_bytes(),
            &[],
        );
        assert!(whitespace_duplicates.rows.iter().all(|row| row.duplicate));
        let accent_distinct = parse_csv("title,authors\nCafé,Alice\nCafe,Alice\n".as_bytes(), &[]);
        assert!(!accent_distinct.rows[0].duplicate);
        assert!(!accent_distinct.rows[1].duplicate);
    }

    #[test]
    fn preview_has_no_writes_and_commit_rechecks_duplicates_without_overwriting() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("library.db");
        let connection = open_database(&path).unwrap();
        let first = parse_csv(b"title,isbn\nFirst,00123\n", &[]);
        let stale = parse_csv(b"title,isbn\nSecond,00-123\n", &[]);
        assert!(list_documents::<Book>(&connection, "books")
            .unwrap()
            .is_empty());
        assert_eq!(
            commit_preview(&connection, &first, false, false).unwrap(),
            1
        );
        let existing: Vec<Book> = list_documents(&connection, "books").unwrap();
        assert!(commit_preview(&connection, &stale, false, false)
            .unwrap_err()
            .starts_with("CSV_DUPLICATES:"));
        assert_eq!(
            list_documents::<Book>(&connection, "books").unwrap().len(),
            1
        );
        assert_eq!(commit_preview(&connection, &stale, true, false).unwrap(), 1);
        let preserved: Book = read_document(&connection, "books", &existing[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(preserved.title, "First");
        drop(connection);
        let reopened = open_database(&path).unwrap();
        let books: Vec<Book> = list_documents(&reopened, "books").unwrap();
        assert_eq!(books.len(), 2);
        assert!(books
            .iter()
            .all(|book| book.schema_version == SCHEMA_VERSION && book.cover_image.is_none()));
        assert!(list_documents::<crate::CopyRecord>(&reopened, "copies")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unknown_columns_require_acknowledgement_and_are_not_imported() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        let preview = parse_csv(
            b"title,cover_image,copy_identifier\nBook,private.png,001\n",
            &[],
        );
        assert_eq!(preview.unknown_columns, ["cover_image", "copy_identifier"]);
        assert!(commit_preview(&connection, &preview, false, false)
            .unwrap_err()
            .starts_with("CSV_UNKNOWN_COLUMNS:"));
        assert_eq!(
            commit_preview(&connection, &preview, false, true).unwrap(),
            1
        );
        let books: Vec<Book> = list_documents(&connection, "books").unwrap();
        assert!(books[0].cover_image.is_none());
        assert!(list_documents::<crate::CopyRecord>(&connection, "copies")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn intra_file_duplicates_require_confirmation() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        let preview = parse_csv(b"title\nSame\nsame\n", &[]);
        assert!(commit_preview(&connection, &preview, false, false).is_err());
        assert!(list_documents::<Book>(&connection, "books")
            .unwrap()
            .is_empty());
        assert_eq!(
            commit_preview(&connection, &preview, true, false).unwrap(),
            2
        );
    }

    #[test]
    fn later_write_failure_rolls_back_every_insert() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_second BEFORE INSERT ON documents
             WHEN NEW.collection = 'books' AND json_extract(NEW.body, '$.title') = 'Reject'
             BEGIN SELECT RAISE(ABORT, 'Simulated disk write failure'); END;",
            )
            .unwrap();
        let preview = parse_csv(b"title\nFirst\nReject\n", &[]);
        assert!(preview.errors.is_empty());
        assert!(commit_preview(&connection, &preview, false, false)
            .unwrap_err()
            .contains("Simulated disk write failure"));
        assert!(list_documents::<Book>(&connection, "books")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn documented_sample_can_be_imported() {
        let preview = parse_csv(include_bytes!("../../examples/books.csv"), &[]);
        assert!(preview.errors.is_empty(), "{:?}", preview.errors);
        assert_eq!(preview.rows.len(), 3);
        assert_eq!(preview.rows[0].book.pages, Some(320));
        assert_eq!(preview.rows[0].book.identifiers, ["00123", "LOCAL-7"]);

        let pipe = parse_csv_delimited(
            include_bytes!("../../examples/books-pipe.csv"),
            &[],
            None,
            String::new(),
        );
        assert!(pipe.errors.is_empty(), "{:?}", pipe.errors);
        assert_eq!(pipe.rows.len(), 3);
        assert_eq!(pipe.rows[0].book.tags, ["Fiction", "Community"]);
    }
}
