use std::{
    collections::HashMap,
    fs,
    io::{Cursor, Read},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{Local, NaiveDate};
use fs2::FileExt;
use image::ImageFormat;
use rusqlite::{backup::Backup, params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
use uuid::Uuid;

mod csv_import;
mod library_archive;
mod open_library;

const SCHEMA_VERSION: i64 = 2;
const MAX_LOAN_PERIOD_DAYS: u32 = 3650;
const MAX_COVER_BYTES: u64 = 10 * 1024 * 1024;
const MAX_CSV_BYTES: usize = 20 * 1024 * 1024;
const MAX_PAGE_SIZE: usize = 100;
const MAX_COVER_REDIRECTS: usize = 5;
const COVER_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_COVER_TITLE_BYTES: usize = 180;

struct AppState {
    connection: Mutex<Connection>,
    database_path: Mutex<PathBuf>,
    library_dir: Mutex<PathBuf>,
    library_open: Mutex<bool>,
    library_lock: Mutex<Option<std::fs::File>>,
    app_settings_path: PathBuf,
    app_language: Mutex<String>,
    books_view: Mutex<String>,
    default_library_dir: PathBuf,
    testing_mode: bool,
    csv_preview: Mutex<Option<csv_import::ImportPreview>>,
    metadata_lookups: Mutex<HashMap<String, open_library::MetadataLookupSession>>,
    cancelled_operations: Mutex<std::collections::HashSet<String>>,
    operation_status: Mutex<std::collections::HashMap<String, OperationStatus>>,
    startup_error: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Book {
    schema_version: i64,
    id: String,
    title: String,
    isbn: Option<String>,
    authors: Vec<String>,
    publisher: Option<String>,
    publication_year: Option<i32>,
    genre: Option<String>,
    #[serde(default)]
    ddc: Option<String>,
    #[serde(default)]
    lcc: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    identifiers: Vec<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    pages: Option<u64>,
    notes: Option<String>,
    cover_image: Option<CoverImage>,
    created_at: String,
    updated_at: String,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CoverImage {
    path: String,
    media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopyRecord {
    schema_version: i64,
    id: String,
    book_id: String,
    copy_identifier: String,
    status: String,
    #[serde(default)]
    location: Option<String>,
    acquisition_date: Option<String>,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Patron {
    schema_version: i64,
    id: String,
    patron_identifier: String,
    name: String,
    contact_information: Option<String>,
    notes: Option<String>,
    active: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Loan {
    schema_version: i64,
    id: String,
    copy_id: String,
    patron_id: String,
    checkout_date: String,
    due_date: String,
    return_date: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    closure_reason: Option<String>,
    #[serde(default)]
    resolution_note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BookView {
    #[serde(flatten)]
    book: Book,
    total_copies: usize,
    available_copies: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CopyView {
    #[serde(flatten)]
    copy: CopyRecord,
    book_title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoanView {
    loan: Loan,
    copy_identifier: String,
    book_title: String,
    patron_name: String,
    copy_status: String,
    overdue: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    schema_version: i64,
    language: String,
    loan_period_days: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    settings: Settings,
    books_view: String,
    database_path: String,
    library_open: bool,
    library_path: Option<String>,
    default_library_path: String,
    startup_error: Option<String>,
    app_data_path: String,
    testing_mode: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationStatus {
    pub(crate) phase: String,
    pub(crate) completed: usize,
    pub(crate) total: usize,
    pub(crate) cancellable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    items: Vec<T>,
    total: usize,
    page: usize,
    page_size: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookQuery {
    #[serde(default)]
    search: String,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    #[serde(default = "default_book_sort", alias = "sort")]
    sort_by: String,
    #[serde(default)]
    descending: bool,
    #[serde(default)]
    available_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorQuery {
    #[serde(default)]
    search: String,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorBookView {
    author: Option<String>,
    book: BookView,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatronQuery {
    #[serde(default)]
    search: String,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    #[serde(default = "default_patron_sort", alias = "sort")]
    sort_by: String,
    #[serde(default)]
    descending: bool,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopyQuery {
    #[serde(default)]
    search: String,
    book_id: Option<String>,
    status: Option<String>,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    #[serde(default = "default_copy_sort", alias = "sort")]
    sort_by: String,
    #[serde(default)]
    descending: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoanQuery {
    #[serde(default)]
    search: String,
    patron_id: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    overdue_only: bool,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    #[serde(default = "default_loan_sort", alias = "sort")]
    sort_by: String,
    #[serde(default)]
    descending: bool,
}

fn default_page() -> usize {
    1
}
fn default_page_size() -> usize {
    50
}
fn default_book_sort() -> String {
    "title".into()
}
fn default_patron_sort() -> String {
    "name".into()
}
fn default_copy_sort() -> String {
    "location".into()
}
fn default_loan_sort() -> String {
    "dueDate".into()
}

fn app_error(error: impl std::fmt::Display) -> String {
    format!("APP_ERROR: {error}")
}

fn validate_date(value: &str) -> Result<NaiveDate, String> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "INVALID_DATE: Use YYYY-MM-DD for dates.".to_owned())?;
    if date.format("%Y-%m-%d").to_string() != value {
        return Err("INVALID_DATE: Use YYYY-MM-DD for dates.".into());
    }
    Ok(date)
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim().to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(crate) fn clean_string_list<T: AsRef<str>>(values: impl IntoIterator<Item = T>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.as_ref().trim().to_owned();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .collect()
}

fn search_key(value: &str) -> String {
    collapse_whitespace(
        &value
            .case_fold()
            .nfkd()
            .filter(|character| !is_combining_mark(*character))
            .nfc()
            .collect::<String>(),
    )
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn identifier_key(value: &str) -> String {
    value.nfc().case_fold().nfc().collect()
}

fn identity_key(value: &str) -> String {
    collapse_whitespace(&value.nfc().case_fold().nfc().collect::<String>())
}

fn normalize_classification(value: Option<String>) -> Option<String> {
    clean_optional(value)
}

fn validate_loan_period(loan_period_days: u32) -> Result<(), String> {
    if !(1..=MAX_LOAN_PERIOD_DAYS).contains(&loan_period_days) {
        return Err(format!(
            "INVALID_LOAN_PERIOD: Loan period must be between 1 and {MAX_LOAN_PERIOD_DAYS} days."
        ));
    }
    Ok(())
}

pub(crate) fn validate_cover_image(bytes: &[u8]) -> Result<(&'static str, &'static str), String> {
    if bytes.len() as u64 > MAX_COVER_BYTES {
        return Err("INVALID_IMAGE_SIZE: Cover images cannot exceed 10 MiB.".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "INVALID_IMAGE: Select a readable JPEG or PNG cover image.".to_owned())?;
    let format = reader
        .format()
        .ok_or_else(|| "INVALID_IMAGE: Select a JPEG or PNG cover image.".to_owned())?;
    let (extension, media_type) = match format {
        ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        ImageFormat::Png => ("png", "image/png"),
        _ => return Err("INVALID_IMAGE: Only JPEG and PNG cover images are supported.".into()),
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8_000);
    limits.max_image_height = Some(8_000);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| "INVALID_IMAGE: The selected image is corrupt or unreadable.".to_owned())?;
    Ok((extension, media_type))
}

fn sanitized_cover_title(title: &str) -> String {
    let mut value = String::new();
    let mut last_replacement = false;
    let mut last_whitespace = false;
    for character in title.trim().chars() {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            if !last_replacement {
                value.push('_');
            }
            last_replacement = true;
            last_whitespace = false;
        } else if character.is_whitespace() {
            if !last_whitespace && !value.is_empty() {
                value.push(' ');
            }
            last_replacement = false;
            last_whitespace = true;
        } else {
            value.push(character);
            last_replacement = false;
            last_whitespace = false;
        }
    }
    while value.ends_with([' ', '.']) {
        value.pop();
    }
    while value.len() > MAX_COVER_TITLE_BYTES {
        value.pop();
    }
    while value.ends_with([' ', '.']) {
        value.pop();
    }
    if value.is_empty() || value.chars().all(|character| character == '_') {
        value.clear();
        value.push_str("book");
    }
    let stem = value.split('.').next().unwrap_or_default();
    if matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        value.insert(0, '_');
    }
    value
}

fn cover_relative_path(title: &str, book_id: &str, extension: &str) -> String {
    format!(
        "covers/{}--{}.{}",
        sanitized_cover_title(title),
        book_id,
        extension
    )
}

fn cover_extension(cover: &CoverImage) -> Result<&'static str, String> {
    match cover.media_type.as_str() {
        "image/jpeg" => Ok("jpg"),
        "image/png" => Ok("png"),
        _ => Err("INVALID_IMAGE: Stored cover has an unsupported media type.".into()),
    }
}

fn now_string() -> String {
    Local::now().to_rfc3339()
}

fn validate_book_fields(
    title: &str,
    publication_year: Option<i32>,
    pages: Option<u64>,
) -> Result<(), String> {
    if title.trim().is_empty() {
        return Err("REQUIRED_TITLE: Enter a book title.".into());
    }
    if publication_year.is_some_and(|year| year < 0) {
        return Err("INVALID_PUBLICATION_YEAR: Enter a non-negative integer year.".into());
    }
    if pages == Some(0) {
        return Err("INVALID_PAGES: Page count must be a positive whole number.".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplicationSettings {
    language: String,
    library_path: Option<String>,
    #[serde(default = "default_books_view")]
    books_view: String,
}

fn default_books_view() -> String {
    "list".into()
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            language: "en".into(),
            library_path: None,
            books_view: default_books_view(),
        }
    }
}

fn read_application_settings(path: &Path) -> Result<ApplicationSettings, String> {
    if !path.exists() {
        return Ok(ApplicationSettings::default());
    }
    let bytes = fs::read(path).map_err(app_error)?;
    let settings: ApplicationSettings = serde_json::from_slice(&bytes).map_err(|error| {
        format!("SETTINGS_CORRUPT: Application settings are unreadable: {error}")
    })?;
    if !["en", "es", "fr"].contains(&settings.language.as_str()) {
        return Err(format!(
            "UNSUPPORTED_LANGUAGE: Configured language '{}' is not supported; select en, es, or fr.",
            settings.language
        ));
    }
    if !["list", "grid"].contains(&settings.books_view.as_str()) {
        return Err(format!(
            "UNSUPPORTED_BOOKS_VIEW: Configured Books view '{}' is not supported; select list or grid.",
            settings.books_view
        ));
    }
    Ok(settings)
}

fn write_application_settings(
    path: &Path,
    language: &str,
    library_path: Option<String>,
    books_view: &str,
) -> Result<(), String> {
    if !["en", "es", "fr"].contains(&language) {
        return Err("UNSUPPORTED_LANGUAGE: Choose English, Spanish, or French.".into());
    }
    if !["list", "grid"].contains(&books_view) {
        return Err("UNSUPPORTED_BOOKS_VIEW: Choose List or Grid.".into());
    }
    let settings = ApplicationSettings {
        language: language.to_owned(),
        library_path,
        books_view: books_view.to_owned(),
    };
    let bytes = serde_json::to_vec_pretty(&settings).map_err(app_error)?;
    fs::write(path, bytes).map_err(app_error)
}

pub(crate) fn current_library_dir(state: &AppState) -> Result<PathBuf, String> {
    state
        .library_dir
        .lock()
        .map(|path| path.clone())
        .map_err(app_error)
}

pub(crate) fn ensure_library_open(state: &AppState) -> Result<(), String> {
    if *state.library_open.lock().map_err(app_error)? {
        Ok(())
    } else {
        Err("LIBRARY_NOT_OPEN: Create or open a library before using catalog commands.".into())
    }
}

pub(crate) fn validate_open_library(
    connection: &Connection,
    library_dir: &Path,
) -> Result<(), String> {
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(app_error)?;
    if integrity != "ok" {
        return Err(format!(
            "LIBRARY_CORRUPT: SQLite integrity check failed: {integrity}"
        ));
    }
    for collection in ["books", "copies", "patrons", "loans"] {
        let mut statement = connection
            .prepare("SELECT id, body FROM documents WHERE collection = ?1 ORDER BY id")
            .map_err(app_error)?;
        let rows = statement
            .query_map([collection], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(app_error)?;
        for row in rows {
            let (id, body) = row.map_err(app_error)?;
            let error =
                |reason: String| format!("LIBRARY_DOCUMENT_INVALID: {collection}/{id}: {reason}");
            match collection {
                "books" => {
                    let document: Book =
                        serde_json::from_str(&body).map_err(|e| error(e.to_string()))?;
                    validate_schema(document.schema_version).map_err(error)?;
                    validate_book_fields(
                        &document.title,
                        document.publication_year,
                        document.pages,
                    )
                    .map_err(error)?;
                }
                "copies" => {
                    let document: CopyRecord =
                        serde_json::from_str(&body).map_err(|e| error(e.to_string()))?;
                    validate_schema(document.schema_version).map_err(error)?;
                    if !["available", "checked_out", "lost", "withdrawn"]
                        .contains(&document.status.as_str())
                    {
                        return Err(error(format!("unsupported status '{}'", document.status)));
                    }
                }
                "patrons" => {
                    let document: Patron =
                        serde_json::from_str(&body).map_err(|e| error(e.to_string()))?;
                    validate_schema(document.schema_version).map_err(error)?;
                }
                "loans" => {
                    let document: Loan =
                        serde_json::from_str(&body).map_err(|e| error(e.to_string()))?;
                    validate_schema(document.schema_version).map_err(error)?;
                    validate_loan(&document).map_err(error)?;
                }
                _ => unreachable!(),
            }
        }
    }
    let books: Vec<Book> = list_documents(connection, "books")?;
    let copies: Vec<CopyRecord> = list_documents(connection, "copies")?;
    let patrons: Vec<Patron> = list_documents(connection, "patrons")?;
    let loans: Vec<Loan> = list_documents(connection, "loans")?;
    let book_ids: std::collections::HashSet<&str> =
        books.iter().map(|book| book.id.as_str()).collect();
    let copy_ids: std::collections::HashSet<&str> =
        copies.iter().map(|copy| copy.id.as_str()).collect();
    let patron_ids: std::collections::HashSet<&str> =
        patrons.iter().map(|patron| patron.id.as_str()).collect();
    let mut active_by_copy: HashMap<&str, usize> = HashMap::new();
    for book in &books {
        if let Some(cover) = &book.cover_image {
            managed_cover_path(library_dir, &cover.path)
                .map_err(|error| format!("LIBRARY_DOCUMENT_INVALID: books/{}: {error}", book.id))?;
        }
    }
    for copy in &copies {
        if !book_ids.contains(copy.book_id.as_str()) {
            return Err(format!(
                "LIBRARY_REFERENCE_INVALID: copies/{} refers to missing book '{}'.",
                copy.id, copy.book_id
            ));
        }
    }
    for loan in &loans {
        if !copy_ids.contains(loan.copy_id.as_str())
            || !patron_ids.contains(loan.patron_id.as_str())
        {
            return Err(format!(
                "LIBRARY_REFERENCE_INVALID: loans/{} refers to a missing copy or patron.",
                loan.id
            ));
        }
        if loan.closed_at.is_none() {
            *active_by_copy.entry(&loan.copy_id).or_default() += 1;
        }
    }
    for copy in &copies {
        let active = active_by_copy.get(copy.id.as_str()).copied().unwrap_or(0);
        let valid = match copy.status.as_str() {
            "available" | "withdrawn" => active == 0,
            "checked_out" => active == 1,
            "lost" => active <= 1,
            _ => false,
        };
        if !valid {
            return Err(format!(
                "LIBRARY_STATE_INVALID: copies/{} status '{}' conflicts with {active} active loans.",
                copy.id, copy.status
            ));
        }
    }
    Ok(())
}

fn open_existing_library(directory: &Path) -> Result<(Connection, std::fs::File), String> {
    if !directory.is_dir() {
        return Err("LIBRARY_NOT_FOUND: Select an existing library directory.".into());
    }
    let database_path = directory.join("library.sqlite3");
    if !database_path.is_file() {
        return Err("LIBRARY_NOT_FOUND: The selected directory has no library.sqlite3.".into());
    }
    if fs::symlink_metadata(&database_path)
        .map_err(app_error)?
        .file_type()
        .is_symlink()
    {
        return Err("LIBRARY_INVALID: library.sqlite3 cannot be a symlink.".into());
    }
    if !directory.join("covers").is_dir() {
        return Err("LIBRARY_INVALID: The selected library has no covers directory.".into());
    }
    let lock_path = directory.join(".library.lock");
    if lock_path.exists()
        && fs::symlink_metadata(&lock_path)
            .map_err(app_error)?
            .file_type()
            .is_symlink()
    {
        return Err("LIBRARY_INVALID: The library lock file cannot be a symlink.".into());
    }
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(app_error)?;
    lock_file.try_lock_exclusive().map_err(|_| {
        "LIBRARY_LOCKED: This library is already open for writing in another application instance."
            .to_owned()
    })?;
    let result = (|| {
        let connection = open_database(&database_path)?;
        validate_open_library(&connection, directory)?;
        reconcile_owned_covers(&connection, directory)?;
        Ok(connection)
    })();
    match result {
        Ok(connection) => Ok((connection, lock_file)),
        Err(error) => {
            let _ = lock_file.unlock();
            Err(error)
        }
    }
}

fn open_database(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(app_error)?;
    }
    let connection = Connection::open(path).map_err(app_error)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(app_error)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         CREATE TABLE IF NOT EXISTS documents (
             collection TEXT NOT NULL,
             id TEXT NOT NULL,
             body TEXT NOT NULL CHECK (json_valid(body)),
             PRIMARY KEY (collection, id)
         );",
        )
        .map_err(app_error)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(app_error)?;
    if version > SCHEMA_VERSION {
        return Err(
            "DATABASE_VERSION_NEWER: This database was created by a newer app version.".into(),
        );
    }
    if version < SCHEMA_VERSION {
        if version > 0 {
            let backup_path = path.with_file_name(format!(
                "{}.pre-migration-v{}.bak",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("library.sqlite3"),
                version
            ));
            if !backup_path.exists() {
                let mut destination = Connection::open(&backup_path).map_err(app_error)?;
                Backup::new(&connection, &mut destination)
                    .map_err(app_error)?
                    .run_to_completion(5, std::time::Duration::from_millis(50), None)
                    .map_err(app_error)?;
            }
        }
        let transaction = connection.unchecked_transaction().map_err(app_error)?;
        transaction
            .execute_batch(
                "DROP INDEX IF EXISTS one_active_loan_per_copy;
             UPDATE documents
             SET body = CASE
                 WHEN collection = 'loans' THEN json_set(
                     json_set(
                         json_set(body, '$.schemaVersion', 2),
                         '$.closedAt', json_extract(body, '$.returnDate')
                     ),
                     '$.closureReason',
                     CASE WHEN json_extract(body, '$.returnDate') IS NULL
                          THEN NULL ELSE 'returned' END
                 )
                 ELSE json_set(body, '$.schemaVersion', 2)
             END;",
            )
            .map_err(app_error)?;
        transaction
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS unique_copy_identifier
                 ON documents(json_extract(body, '$.copyIdentifier'))
                 WHERE collection = 'copies';
             CREATE UNIQUE INDEX IF NOT EXISTS unique_patron_identifier
                 ON documents(json_extract(body, '$.patronIdentifier'))
                 WHERE collection = 'patrons';
             CREATE UNIQUE INDEX one_active_loan_per_copy
                 ON documents(json_extract(body, '$.copyId'))
                 WHERE collection = 'loans'
                   AND json_extract(body, '$.closedAt') IS NULL;
             CREATE INDEX IF NOT EXISTS book_title_index
                 ON documents(json_extract(body, '$.title'))
                 WHERE collection = 'books';
             CREATE INDEX IF NOT EXISTS book_isbn_index
                 ON documents(json_extract(body, '$.isbn'))
                 WHERE collection = 'books';
             CREATE INDEX IF NOT EXISTS book_ddc_index
                 ON documents(json_extract(body, '$.ddc'))
                 WHERE collection = 'books';
             CREATE INDEX IF NOT EXISTS book_lcc_index
                 ON documents(json_extract(body, '$.lcc'))
                 WHERE collection = 'books';
             CREATE INDEX IF NOT EXISTS patron_name_index
                 ON documents(json_extract(body, '$.name'))
                 WHERE collection = 'patrons';
             CREATE INDEX IF NOT EXISTS copy_book_index
                 ON documents(json_extract(body, '$.bookId'))
                 WHERE collection = 'copies';
             CREATE INDEX IF NOT EXISTS copy_location_index
                 ON documents(json_extract(body, '$.location'))
                 WHERE collection = 'copies';
             CREATE INDEX IF NOT EXISTS copy_status_index
                 ON documents(json_extract(body, '$.status'))
                 WHERE collection = 'copies';
             CREATE INDEX IF NOT EXISTS loan_copy_index
                 ON documents(json_extract(body, '$.copyId'))
                 WHERE collection = 'loans';
             CREATE INDEX IF NOT EXISTS loan_patron_index
                 ON documents(json_extract(body, '$.patronId'))
                 WHERE collection = 'loans';
             CREATE INDEX IF NOT EXISTS loan_due_date_index
                 ON documents(json_extract(body, '$.dueDate'))
                 WHERE collection = 'loans';
             CREATE INDEX IF NOT EXISTS loan_closed_at_index
                 ON documents(json_extract(body, '$.closedAt'))
                 WHERE collection = 'loans';
             CREATE INDEX IF NOT EXISTS loan_return_date_index
                 ON documents(json_extract(body, '$.returnDate'))
                 WHERE collection = 'loans';",
            )
            .map_err(app_error)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(app_error)?;
        transaction.commit().map_err(app_error)?;
    }
    Ok(connection)
}

fn read_document<T: for<'de> Deserialize<'de>>(
    connection: &Connection,
    collection: &str,
    id: &str,
) -> Result<Option<T>, String> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM documents WHERE collection = ?1 AND id = ?2",
            params![collection, id],
            |row| row.get(0),
        )
        .optional()
        .map_err(app_error)?;
    body.map(|json| serde_json::from_str(&json).map_err(app_error))
        .transpose()
}

fn read_document_tx<T: for<'de> Deserialize<'de>>(
    transaction: &Transaction<'_>,
    collection: &str,
    id: &str,
) -> Result<Option<T>, String> {
    let body: Option<String> = transaction
        .query_row(
            "SELECT body FROM documents WHERE collection = ?1 AND id = ?2",
            params![collection, id],
            |row| row.get(0),
        )
        .optional()
        .map_err(app_error)?;
    body.map(|json| serde_json::from_str(&json).map_err(app_error))
        .transpose()
}

fn list_documents<T: for<'de> Deserialize<'de>>(
    connection: &Connection,
    collection: &str,
) -> Result<Vec<T>, String> {
    let mut statement = connection
        .prepare("SELECT body FROM documents WHERE collection = ?1 ORDER BY id")
        .map_err(app_error)?;
    let rows = statement
        .query_map([collection], |row| row.get::<_, String>(0))
        .map_err(app_error)?;
    rows.map(|row| {
        let body = row.map_err(app_error)?;
        serde_json::from_str(&body).map_err(app_error)
    })
    .collect()
}

fn save_document<T: Serialize>(
    transaction: &Transaction<'_>,
    collection: &str,
    id: &str,
    value: &T,
) -> Result<(), String> {
    let body = serde_json::to_string(value).map_err(app_error)?;
    transaction
        .execute(
            "INSERT INTO documents(collection, id, body) VALUES (?1, ?2, ?3)
             ON CONFLICT(collection, id) DO UPDATE SET body = excluded.body",
            params![collection, id, body],
        )
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("unique_copy_identifier")
                || message.contains("unique_patron_identifier")
            {
                "DUPLICATE_IDENTIFIER: That identifier is already in use.".into()
            } else {
                app_error(message)
            }
        })?;
    Ok(())
}

fn default_settings() -> Settings {
    Settings {
        schema_version: SCHEMA_VERSION,
        language: "en".into(),
        loan_period_days: 21,
    }
}

fn get_settings(connection: &Connection) -> Result<Settings, String> {
    let settings = read_document(connection, "settings", "app")?.unwrap_or_else(default_settings);
    validate_loan_period(settings.loan_period_days)?;
    Ok(settings)
}

fn get_copy(connection: &Connection, id: &str) -> Result<CopyRecord, String> {
    read_document(connection, "copies", id)?.ok_or_else(|| "COPY_NOT_FOUND: Copy not found.".into())
}

fn get_book(connection: &Connection, id: &str) -> Result<Book, String> {
    read_document(connection, "books", id)?.ok_or_else(|| "BOOK_NOT_FOUND: Book not found.".into())
}

fn get_patron(connection: &Connection, id: &str) -> Result<Patron, String> {
    read_document(connection, "patrons", id)?
        .ok_or_else(|| "PATRON_NOT_FOUND: Patron not found.".into())
}

fn validate_schema(version: i64) -> Result<(), String> {
    if version != SCHEMA_VERSION {
        Err("DOCUMENT_VERSION: Unsupported document schema version.".into())
    } else {
        Ok(())
    }
}

fn is_overdue(loan: &Loan, today: NaiveDate) -> Result<bool, String> {
    if loan.closed_at.is_some() {
        return Ok(false);
    }
    Ok(validate_date(&loan.due_date)? < today)
}

fn validate_loan(loan: &Loan) -> Result<(), String> {
    let checkout = validate_date(&loan.checkout_date)?;
    let due = validate_date(&loan.due_date)?;
    if due < checkout {
        return Err("INVALID_DUE_DATE: Due date cannot be before checkout.".into());
    }
    match (loan.closed_at.as_deref(), loan.closure_reason.as_deref()) {
        (None, None) if loan.return_date.is_none() && loan.resolution_note.is_none() => Ok(()),
        (None, _) => {
            Err("INVALID_LOAN_CLOSURE: An active loan cannot have closure details.".into())
        }
        (Some(closed_at), Some("returned")) => {
            let returned = loan.return_date.as_deref().ok_or_else(|| {
                "INVALID_LOAN_CLOSURE: Returned loans require a return date.".to_owned()
            })?;
            if returned != closed_at {
                return Err("INVALID_LOAN_CLOSURE: Return and closure dates must match.".into());
            }
            if validate_date(closed_at)? < checkout {
                return Err("INVALID_LOAN_CLOSURE: Closure cannot precede checkout.".into());
            }
            if loan.resolution_note.is_some() {
                return Err(
                    "INVALID_LOAN_CLOSURE: Returned loans cannot have resolution notes.".into(),
                );
            }
            Ok(())
        }
        (Some(closed_at), Some("lost_resolved")) => {
            if loan.return_date.is_some()
                || clean_optional(loan.resolution_note.clone()).is_none()
                || validate_date(closed_at)? < checkout
            {
                return Err("INVALID_LOAN_CLOSURE: Lost resolutions require a date and note, no return date, and cannot precede checkout.".into());
            }
            Ok(())
        }
        (Some(_), _) => Err("INVALID_LOAN_CLOSURE: Unsupported loan closure reason.".into()),
    }
}

#[tauri::command]
fn app_info(state: State<'_, AppState>) -> Result<AppInfo, String> {
    let library_open = *state.library_open.lock().map_err(app_error)?;
    let settings = if library_open {
        let connection = state.connection.lock().map_err(app_error)?;
        get_settings(&connection)?
    } else {
        default_settings()
    };
    let mut settings = settings;
    settings.language = state.app_language.lock().map_err(app_error)?.clone();
    let database_path = state.database_path.lock().map_err(app_error)?;
    let library_dir = state.library_dir.lock().map_err(app_error)?;
    Ok(AppInfo {
        settings,
        books_view: state.books_view.lock().map_err(app_error)?.clone(),
        database_path: if library_open {
            database_path.display().to_string()
        } else {
            String::new()
        },
        library_open,
        library_path: library_open.then(|| library_dir.display().to_string()),
        default_library_path: state.default_library_dir.display().to_string(),
        startup_error: state.startup_error.lock().map_err(app_error)?.clone(),
        app_data_path: state
            .app_settings_path
            .parent()
            .unwrap_or(Path::new(""))
            .display()
            .to_string(),
        testing_mode: state.testing_mode,
    })
}

#[tauri::command]
fn update_settings(
    state: State<'_, AppState>,
    language: String,
    loan_period_days: u32,
) -> Result<Settings, String> {
    if !["en", "es", "fr"].contains(&language.as_str()) {
        return Err("UNSUPPORTED_LANGUAGE: Choose English, Spanish, or French.".into());
    }
    validate_loan_period(loan_period_days)?;
    let library_open = *state.library_open.lock().map_err(app_error)?;
    let mut settings = if library_open {
        let connection = state.connection.lock().map_err(app_error)?;
        let mut settings = get_settings(&connection)?;
        settings.loan_period_days = loan_period_days;
        let tx = connection.unchecked_transaction().map_err(app_error)?;
        save_document(&tx, "settings", "app", &settings)?;
        tx.commit().map_err(app_error)?;
        settings
    } else {
        default_settings()
    };
    let library_path = if library_open {
        Some(current_library_dir(&state)?.display().to_string())
    } else {
        None
    };
    let books_view = state.books_view.lock().map_err(app_error)?.clone();
    write_application_settings(
        &state.app_settings_path,
        &language,
        library_path,
        &books_view,
    )?;
    *state.app_language.lock().map_err(app_error)? = language.clone();
    settings.language = language;
    Ok(settings)
}

#[tauri::command]
fn update_books_view(state: State<'_, AppState>, books_view: String) -> Result<String, String> {
    if !["list", "grid"].contains(&books_view.as_str()) {
        return Err("UNSUPPORTED_BOOKS_VIEW: Choose List or Grid.".into());
    }
    let language = state.app_language.lock().map_err(app_error)?.clone();
    let library_path = if *state.library_open.lock().map_err(app_error)? {
        Some(current_library_dir(&state)?.display().to_string())
    } else {
        None
    };
    write_application_settings(
        &state.app_settings_path,
        &language,
        library_path,
        &books_view,
    )?;
    *state.books_view.lock().map_err(app_error)? = books_view.clone();
    Ok(books_view)
}

#[tauri::command]
fn switch_database(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let path = PathBuf::from(path);
    let directory = if path
        .file_name()
        .is_some_and(|name| name == "library.sqlite3")
    {
        path.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        path
    };
    open_library_internal(&state, &directory)?;
    Ok(directory.join("library.sqlite3").display().to_string())
}

fn open_library_internal(state: &AppState, directory: &Path) -> Result<(), String> {
    let directory = directory.canonicalize().map_err(app_error)?;
    if *state.library_open.lock().map_err(app_error)?
        && state.database_path.lock().map_err(app_error)?.as_path()
            == directory.join("library.sqlite3").as_path()
    {
        return Ok(());
    }
    let (connection, lock_file) = open_existing_library(&directory)?;
    let language = state.app_language.lock().map_err(app_error)?.clone();
    let books_view = state.books_view.lock().map_err(app_error)?.clone();
    write_application_settings(
        &state.app_settings_path,
        &language,
        Some(directory.display().to_string()),
        &books_view,
    )?;
    *state.connection.lock().map_err(app_error)? = connection;
    *state.database_path.lock().map_err(app_error)? = directory.join("library.sqlite3");
    *state.library_dir.lock().map_err(app_error)? = directory;
    let mut old_lock = state.library_lock.lock().map_err(app_error)?;
    *old_lock = Some(lock_file);
    *state.library_open.lock().map_err(app_error)? = true;
    *state.csv_preview.lock().map_err(app_error)? = None;
    state.metadata_lookups.lock().map_err(app_error)?.clear();
    *state.startup_error.lock().map_err(app_error)? = None;
    Ok(())
}

#[tauri::command]
fn open_library(state: State<'_, AppState>, path: String) -> Result<AppInfo, String> {
    ensure_abs_path(&path)?;
    open_library_internal(&state, Path::new(&path))?;
    app_info(state)
}

#[tauri::command]
fn create_library(state: State<'_, AppState>, path: String) -> Result<AppInfo, String> {
    let directory = PathBuf::from(path);
    ensure_abs_path(directory.to_str().unwrap_or_default())?;
    if directory.exists() {
        if !directory.is_dir()
            || fs::read_dir(&directory)
                .map_err(app_error)?
                .next()
                .is_some()
        {
            return Err("LIBRARY_DIRECTORY_NOT_EMPTY: Choose a new or empty directory.".into());
        }
    } else {
        fs::create_dir(&directory).map_err(app_error)?;
    }
    fs::create_dir_all(directory.join("covers")).map_err(app_error)?;
    let database_path = directory.join("library.sqlite3");
    if database_path.exists() {
        return Err("LIBRARY_ALREADY_EXISTS: Refusing to replace an existing library.".into());
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&database_path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                "LIBRARY_ALREADY_EXISTS: Refusing to replace an existing library.".to_owned()
            } else {
                app_error(error)
            }
        })?;
    let connection = open_database(&database_path)?;
    drop(connection);
    open_library_internal(&state, &directory)?;
    app_info(state)
}

fn ensure_abs_path(path: &str) -> Result<(), String> {
    if Path::new(path).is_absolute() {
        Ok(())
    } else {
        Err("INVALID_LIBRARY_PATH: Choose an absolute library directory.".into())
    }
}

#[tauri::command]
fn operation_status(
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<Option<OperationStatus>, String> {
    state
        .operation_status
        .lock()
        .map_err(app_error)
        .map(|operations| operations.get(&operation_id).cloned())
}

#[tauri::command]
fn cancel_operation(state: State<'_, AppState>, operation_id: String) -> Result<(), String> {
    let operations = state.operation_status.lock().map_err(app_error)?;
    if operations
        .get(&operation_id)
        .is_some_and(|status| !status.cancellable)
    {
        return Err("OPERATION_COMMITTING: The final transaction is committing and can no longer be cancelled.".into());
    }
    state
        .cancelled_operations
        .lock()
        .map_err(app_error)?
        .insert(operation_id);
    Ok(())
}

#[tauri::command]
fn list_books(state: State<'_, AppState>, search: String) -> Result<Vec<BookView>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let books: Vec<Book> = list_documents(&connection, "books")?;
    let copies: Vec<CopyRecord> = list_documents(&connection, "copies")?;
    let mut copies_by_book: HashMap<&str, Vec<&CopyRecord>> = HashMap::new();
    for copy in &copies {
        copies_by_book.entry(&copy.book_id).or_default().push(copy);
    }
    let needle = search_key(search.trim());
    let identifier_needle = identifier_key(search.trim());
    let isbn_needle = normalized_isbn(search.trim());
    let classification_needle = identifier_key(search.trim());
    books
        .into_iter()
        .filter(|book| {
            needle.is_empty()
                || search_key(&book.title).contains(&needle)
                || book
                    .authors
                    .iter()
                    .any(|author| search_key(author).contains(&needle))
                || (!isbn_needle.is_empty()
                    && normalized_isbn(book.isbn.as_deref().unwrap_or_default())
                        .contains(&isbn_needle))
                || book
                    .ddc
                    .as_deref()
                    .is_some_and(|value| identifier_key(value).contains(&classification_needle))
                || book
                    .lcc
                    .as_deref()
                    .is_some_and(|value| identifier_key(value).contains(&classification_needle))
                || book
                    .tags
                    .iter()
                    .any(|value| search_key(value).contains(&needle))
                || book
                    .identifiers
                    .iter()
                    .any(|value| identifier_key(value).contains(&identifier_needle))
                || book
                    .language
                    .as_deref()
                    .is_some_and(|value| search_key(value).contains(&needle))
                || copies_by_book
                    .get(book.id.as_str())
                    .into_iter()
                    .flatten()
                    .any(|copy| {
                        identifier_key(&copy.copy_identifier).contains(&identifier_needle)
                            || search_key(copy.location.as_deref().unwrap_or_default())
                                .contains(&needle)
                    })
        })
        .map(|book| {
            validate_schema(book.schema_version)?;
            let related: Vec<&CopyRecord> = copies_by_book
                .get(book.id.as_str())
                .cloned()
                .unwrap_or_default();
            Ok(BookView {
                total_copies: related.len(),
                available_copies: related
                    .iter()
                    .filter(|copy| copy.status == "available")
                    .count(),
                book,
            })
        })
        .collect()
}

#[tauri::command]
fn save_book(
    state: State<'_, AppState>,
    id: Option<String>,
    title: String,
    isbn: Option<String>,
    authors: Vec<String>,
    publisher: Option<String>,
    publication_year: Option<i32>,
    genre: Option<String>,
    ddc: Option<String>,
    lcc: Option<String>,
    tags: Vec<String>,
    identifiers: Vec<String>,
    language: Option<String>,
    pages: Option<u64>,
    notes: Option<String>,
) -> Result<Book, String> {
    ensure_library_open(&state)?;
    let title = title.trim().to_owned();
    validate_book_fields(&title, publication_year, pages)?;
    let library_dir = current_library_dir(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let previous: Option<Book> = read_document(&connection, "books", &id)?;
    let previous_cover = previous
        .as_ref()
        .and_then(|value| value.cover_image.clone());
    let extra = previous
        .as_ref()
        .map(|book| book.extra.clone())
        .unwrap_or_default();
    let now = now_string();
    let book = Book {
        schema_version: SCHEMA_VERSION,
        id: id.clone(),
        title,
        isbn: clean_optional(isbn),
        authors: authors
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        publisher: clean_optional(publisher),
        publication_year,
        genre: clean_optional(genre),
        ddc: normalize_classification(ddc),
        lcc: normalize_classification(lcc),
        tags: clean_string_list(tags),
        identifiers: clean_string_list(identifiers),
        language: clean_optional(language),
        pages,
        notes: clean_optional(notes),
        cover_image: previous_cover.clone(),
        created_at: previous
            .map(|value| value.created_at)
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
        extra,
    };
    let (book, previous_cover_path) =
        persist_book_update(&connection, &library_dir, previous_cover.as_ref(), book)?;
    drop(connection);
    if let Some(previous_path) = previous_cover_path {
        cleanup_cover_if_unreferenced(&state, &library_dir, &previous_path)?;
    }
    Ok(book)
}

fn persist_book_update(
    connection: &Connection,
    library_dir: &Path,
    previous_cover: Option<&CoverImage>,
    mut book: Book,
) -> Result<(Book, Option<String>), String> {
    let renamed_cover = if let Some(cover) = previous_cover {
        let relative = cover_relative_path(&book.title, &book.id, cover_extension(cover)?);
        if relative != cover.path {
            let source = managed_cover_path(library_dir, &cover.path)?;
            let destination = managed_cover_path(library_dir, &relative)?;
            let mut input = fs::File::open(source).map_err(app_error)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(app_error)?;
            if let Err(error) = std::io::copy(&mut input, &mut output) {
                let _ = fs::remove_file(&destination);
                return Err(app_error(error));
            }
            if let Err(error) = output.sync_all() {
                let _ = fs::remove_file(&destination);
                return Err(app_error(error));
            }
            book.cover_image = Some(CoverImage {
                path: relative.clone(),
                media_type: cover.media_type.clone(),
            });
            Some((cover.path.clone(), relative))
        } else {
            None
        }
    } else {
        None
    };
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    let commit_result =
        save_document(&tx, "books", &book.id, &book).and_then(|()| tx.commit().map_err(app_error));
    if let Err(error) = commit_result {
        if let Some((_, staged)) = renamed_cover.as_ref() {
            let _ = remove_managed_cover(library_dir, staged);
        }
        return Err(error);
    }
    Ok((book, renamed_cover.map(|(previous, _)| previous)))
}

#[tauri::command]
fn delete_book(state: State<'_, AppState>, id: String) -> Result<(), String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let copies: Vec<CopyRecord> = list_documents(&connection, "copies")?;
    if copies.iter().any(|copy| copy.book_id == id) {
        return Err(
            "BOOK_HAS_HISTORY: Remove its copies and preserve loan history before deleting.".into(),
        );
    }
    let book = get_book(&connection, &id)?;
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    tx.execute(
        "DELETE FROM documents WHERE collection='books' AND id=?1",
        [&id],
    )
    .map_err(app_error)?;
    tx.commit().map_err(app_error)?;
    drop(connection);
    if let Some(cover) = book.cover_image {
        cleanup_cover_if_unreferenced(&state, &current_library_dir(&state)?, &cover.path)?;
    }
    Ok(())
}

#[tauri::command]
fn list_copies(state: State<'_, AppState>, book_id: String) -> Result<Vec<CopyRecord>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    Ok(list_documents::<CopyRecord>(&connection, "copies")?
        .into_iter()
        .filter(|copy| copy.book_id == book_id)
        .collect())
}

#[tauri::command]
fn delete_copy(state: State<'_, AppState>, id: String) -> Result<(), String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let copy = get_copy(&connection, &id)?;
    let loans: Vec<Loan> = list_documents(&connection, "loans")?;
    if loans.iter().any(|loan| loan.copy_id == id) {
        return Err("COPY_HAS_HISTORY: A copy with loan history cannot be deleted.".into());
    }
    if copy.status == "checked_out" {
        return Err("COPY_ON_LOAN: Return this copy before deleting it.".into());
    }
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    tx.execute(
        "DELETE FROM documents WHERE collection='copies' AND id=?1",
        [&id],
    )
    .map_err(app_error)?;
    tx.commit().map_err(app_error)?;
    Ok(())
}

#[tauri::command]
fn add_copy(
    state: State<'_, AppState>,
    book_id: String,
    copy_identifier: String,
    acquisition_date: Option<String>,
    location: Option<String>,
    notes: Option<String>,
) -> Result<CopyRecord, String> {
    ensure_library_open(&state)?;
    let copy_identifier = copy_identifier.trim().to_owned();
    if copy_identifier.is_empty() {
        return Err("REQUIRED_COPY_ID: Enter a copy identifier.".into());
    }
    if let Some(date) = acquisition_date.as_deref() {
        validate_date(date)?;
    }
    let connection = state.connection.lock().map_err(app_error)?;
    get_book(&connection, &book_id)?;
    let now = now_string();
    let copy = CopyRecord {
        schema_version: SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        book_id,
        copy_identifier,
        status: "available".into(),
        location: clean_optional(location),
        acquisition_date,
        notes: clean_optional(notes),
        created_at: now.clone(),
        updated_at: now,
    };
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    save_document(&tx, "copies", &copy.id, &copy)?;
    tx.commit().map_err(app_error)?;
    Ok(copy)
}

#[tauri::command]
fn update_copy(
    state: State<'_, AppState>,
    id: String,
    copy_identifier: String,
    acquisition_date: Option<String>,
    location: Option<String>,
    notes: Option<String>,
) -> Result<CopyRecord, String> {
    ensure_library_open(&state)?;
    let copy_identifier = copy_identifier.trim().to_owned();
    if copy_identifier.is_empty() {
        return Err("REQUIRED_COPY_ID: Enter a copy identifier.".into());
    }
    if let Some(date) = acquisition_date.as_deref() {
        validate_date(date)?;
    }
    let connection = state.connection.lock().map_err(app_error)?;
    let mut copy = get_copy(&connection, &id)?;
    copy.copy_identifier = copy_identifier;
    copy.location = clean_optional(location);
    copy.acquisition_date = acquisition_date;
    copy.notes = clean_optional(notes);
    copy.updated_at = now_string();
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    save_document(&tx, "copies", &id, &copy)?;
    tx.commit().map_err(app_error)?;
    Ok(copy)
}

#[tauri::command]
fn update_copy_status(
    state: State<'_, AppState>,
    id: String,
    status: String,
) -> Result<CopyRecord, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    let copy = change_copy_status_in_transaction(&tx, &id, &status)?;
    tx.commit().map_err(app_error)?;
    Ok(copy)
}

fn change_copy_status_in_transaction(
    tx: &Transaction<'_>,
    id: &str,
    status: &str,
) -> Result<CopyRecord, String> {
    if !["available", "lost", "withdrawn", "checked_out"].contains(&status) {
        return Err(
            "INVALID_COPY_STATUS: Status changes must use the supported circulation actions."
                .into(),
        );
    }
    let mut copy = read_document_tx::<CopyRecord>(tx, "copies", id)?
        .ok_or_else(|| "COPY_NOT_FOUND: Copy not found.".to_owned())?;
    let loans: Vec<Loan> = list_documents(tx, "loans")?;
    let active = loans
        .iter()
        .find(|loan| loan.copy_id == id && loan.closed_at.is_none());
    match (copy.status.as_str(), status, active.is_some()) {
        ("available", "lost", false) | ("available", "withdrawn", false) => {}
        ("checked_out", "lost", true) => {}
        ("lost", "checked_out", true) => {}
        ("lost", "available", false) => {}
        ("lost", "withdrawn", false) => {}
        ("withdrawn", _, _) => {
            return Err("COPY_WITHDRAWN: A withdrawn copy cannot re-enter circulation.".into());
        }
        (_, "withdrawn", true) | (_, "available", true) => {
            return Err("COPY_ON_LOAN: Resolve or physically return the active loan first.".into());
        }
        ("checked_out", "available", _) => {
            return Err(
                "COPY_ON_LOAN: Record a physical return to make the copy available.".into(),
            );
        }
        ("checked_out", "checked_out", true) | ("lost", "lost", _) => return Ok(copy),
        _ => {
            return Err("INVALID_COPY_TRANSITION: Use checkout, return, loss, recovery, or withdrawal actions.".into());
        }
    }
    copy.status = status.to_owned();
    copy.updated_at = now_string();
    save_document(tx, "copies", &copy.id, &copy)?;
    Ok(copy)
}

#[tauri::command]
fn list_patrons(state: State<'_, AppState>, search: String) -> Result<Vec<Patron>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let needle = search_key(search.trim());
    let identifier_needle = identifier_key(search.trim());
    Ok(list_documents::<Patron>(&connection, "patrons")?
        .into_iter()
        .filter(|patron| {
            needle.is_empty()
                || search_key(&patron.name).contains(&needle)
                || identifier_key(&patron.patron_identifier).contains(&identifier_needle)
        })
        .collect())
}

#[tauri::command]
fn save_patron(
    state: State<'_, AppState>,
    id: Option<String>,
    patron_identifier: String,
    name: String,
    contact_information: Option<String>,
    notes: Option<String>,
) -> Result<Patron, String> {
    ensure_library_open(&state)?;
    let patron_identifier = patron_identifier.trim().to_owned();
    let name = name.trim().to_owned();
    if patron_identifier.is_empty() || name.is_empty() {
        return Err("REQUIRED_PATRON_FIELDS: Enter a patron name and identifier.".into());
    }
    let connection = state.connection.lock().map_err(app_error)?;
    let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let previous: Option<Patron> = read_document(&connection, "patrons", &id)?;
    let now = now_string();
    let patron = Patron {
        schema_version: SCHEMA_VERSION,
        id: id.clone(),
        patron_identifier,
        name,
        contact_information: clean_optional(contact_information),
        notes: clean_optional(notes),
        active: previous.as_ref().map(|value| value.active).unwrap_or(true),
        created_at: previous
            .map(|value| value.created_at)
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    save_document(&tx, "patrons", &id, &patron)?;
    tx.commit().map_err(app_error)?;
    Ok(patron)
}

#[tauri::command]
fn deactivate_patron(state: State<'_, AppState>, id: String) -> Result<Patron, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let mut patron = get_patron(&connection, &id)?;
    patron.active = false;
    patron.updated_at = now_string();
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    save_document(&tx, "patrons", &id, &patron)?;
    tx.commit().map_err(app_error)?;
    Ok(patron)
}

#[tauri::command]
fn checkout_in_transaction(
    tx: &Transaction<'_>,
    copy_id: &str,
    patron_id: &str,
    due_date: Option<&str>,
    checkout_date: NaiveDate,
) -> Result<Loan, String> {
    let mut copy = read_document_tx::<CopyRecord>(&tx, "copies", &copy_id)?
        .ok_or_else(|| "COPY_NOT_FOUND: Copy not found.".to_owned())?;
    let patron = read_document_tx::<Patron>(&tx, "patrons", &patron_id)?
        .ok_or_else(|| "PATRON_NOT_FOUND: Patron not found.".to_owned())?;
    validate_schema(copy.schema_version)?;
    validate_schema(patron.schema_version)?;
    if copy.status != "available" {
        return Err("COPY_UNAVAILABLE: This copy is not available.".into());
    }
    if !patron.active {
        return Err("PATRON_INACTIVE: This patron is inactive.".into());
    }
    let settings: Settings =
        read_document_tx(&tx, "settings", "app")?.unwrap_or_else(default_settings);
    let due = match due_date {
        Some(value) => validate_date(value)?,
        None => checkout_date + chrono::Duration::days(settings.loan_period_days as i64),
    };
    if due < checkout_date {
        return Err("INVALID_DUE_DATE: Due date cannot be before checkout.".into());
    }
    let loan = Loan {
        schema_version: SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        copy_id: copy.id.clone(),
        patron_id: patron.id,
        checkout_date: checkout_date.to_string(),
        due_date: due.to_string(),
        return_date: None,
        closed_at: None,
        closure_reason: None,
        resolution_note: None,
    };
    validate_loan(&loan)?;
    copy.status = "checked_out".into();
    copy.updated_at = now_string();
    save_document(&tx, "copies", &copy.id, &copy)?;
    save_document(&tx, "loans", &loan.id, &loan)?;
    Ok(loan)
}

#[tauri::command]
fn checkout(
    state: State<'_, AppState>,
    copy_id: String,
    patron_id: String,
    due_date: Option<String>,
) -> Result<Loan, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    let loan = checkout_in_transaction(
        &tx,
        &copy_id,
        &patron_id,
        due_date.as_deref(),
        Local::now().date_naive(),
    )?;
    tx.commit().map_err(app_error)?;
    Ok(loan)
}

fn return_in_transaction(
    tx: &Transaction<'_>,
    loan_id: &str,
    returned: NaiveDate,
) -> Result<Loan, String> {
    let mut loan = read_document_tx::<Loan>(tx, "loans", loan_id)?
        .ok_or_else(|| "LOAN_NOT_FOUND: Loan not found.".to_owned())?;
    if loan.closed_at.is_some() {
        return Err("LOAN_ALREADY_RETURNED: This loan has already been returned.".into());
    }
    let mut copy = read_document_tx::<CopyRecord>(tx, "copies", &loan.copy_id)?
        .ok_or_else(|| "COPY_NOT_FOUND: Loan copy not found.".to_owned())?;
    loan.return_date = Some(returned.to_string());
    loan.closed_at = Some(returned.to_string());
    loan.closure_reason = Some("returned".into());
    loan.resolution_note = None;
    copy.status = "available".into();
    copy.updated_at = now_string();
    validate_loan(&loan)?;
    save_document(tx, "loans", &loan.id, &loan)?;
    save_document(tx, "copies", &copy.id, &copy)?;
    Ok(loan)
}

#[tauri::command]
fn return_loan(state: State<'_, AppState>, loan_id: String) -> Result<Loan, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    let loan = return_in_transaction(&tx, &loan_id, Local::now().date_naive())?;
    tx.commit().map_err(app_error)?;
    Ok(loan)
}

#[tauri::command]
fn resolve_lost_loan(
    state: State<'_, AppState>,
    loan_id: String,
    note: String,
) -> Result<Loan, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    let loan = resolve_lost_loan_in_transaction(&tx, &loan_id, &note, Local::now().date_naive())?;
    tx.commit().map_err(app_error)?;
    Ok(loan)
}

fn resolve_lost_loan_in_transaction(
    tx: &Transaction<'_>,
    loan_id: &str,
    note: &str,
    resolved: NaiveDate,
) -> Result<Loan, String> {
    let note = clean_optional(Some(note.to_owned())).ok_or_else(|| {
        "RESOLUTION_NOTE_REQUIRED: Enter a note explaining why the loan will not be returned."
            .to_owned()
    })?;
    let mut loan = read_document_tx::<Loan>(tx, "loans", loan_id)?
        .ok_or_else(|| "LOAN_NOT_FOUND: Loan not found.".to_owned())?;
    if loan.closed_at.is_some() {
        return Err("LOAN_ALREADY_CLOSED: This loan has already been resolved.".into());
    }
    let mut copy = read_document_tx::<CopyRecord>(tx, "copies", &loan.copy_id)?
        .ok_or_else(|| "COPY_NOT_FOUND: Loan copy not found.".to_owned())?;
    if copy.status != "lost" {
        return Err("COPY_NOT_LOST: Only an active loan for a lost copy can be resolved.".into());
    }
    loan.closed_at = Some(resolved.to_string());
    loan.closure_reason = Some("lost_resolved".into());
    loan.return_date = None;
    loan.resolution_note = Some(note);
    copy.status = "lost".into();
    copy.updated_at = now_string();
    validate_loan(&loan)?;
    save_document(tx, "loans", &loan.id, &loan)?;
    save_document(tx, "copies", &copy.id, &copy)?;
    Ok(loan)
}

#[tauri::command]
fn list_loans(state: State<'_, AppState>, active_only: bool) -> Result<Vec<LoanView>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let loans: Vec<Loan> = list_documents(&connection, "loans")?;
    let copies: Vec<CopyRecord> = list_documents(&connection, "copies")?;
    let books: Vec<Book> = list_documents(&connection, "books")?;
    let patrons: Vec<Patron> = list_documents(&connection, "patrons")?;
    let copies_by_id: HashMap<&str, &CopyRecord> =
        copies.iter().map(|copy| (copy.id.as_str(), copy)).collect();
    let books_by_id: HashMap<&str, &Book> =
        books.iter().map(|book| (book.id.as_str(), book)).collect();
    let patrons_by_id: HashMap<&str, &Patron> = patrons
        .iter()
        .map(|patron| (patron.id.as_str(), patron))
        .collect();
    let today = Local::now().date_naive();
    loans
        .into_iter()
        .filter(|loan| !active_only || loan.closed_at.is_none())
        .map(|loan| {
            validate_schema(loan.schema_version)?;
            validate_loan(&loan)?;
            let copy = copies_by_id
                .get(loan.copy_id.as_str())
                .copied()
                .ok_or_else(|| "DANGLING_REFERENCE: Loan refers to a missing copy.".to_owned())?;
            let book = books_by_id
                .get(copy.book_id.as_str())
                .copied()
                .ok_or_else(|| "DANGLING_REFERENCE: Copy refers to a missing book.".to_owned())?;
            let patron = patrons_by_id
                .get(loan.patron_id.as_str())
                .copied()
                .ok_or_else(|| "DANGLING_REFERENCE: Loan refers to a missing patron.".to_owned())?;
            Ok(LoanView {
                overdue: is_overdue(&loan, today)?,
                loan,
                copy_identifier: copy.copy_identifier.clone(),
                book_title: book.title.clone(),
                patron_name: patron.name.clone(),
                copy_status: copy.status.clone(),
            })
        })
        .collect()
}

fn validate_pagination(page: usize, page_size: usize) -> Result<(usize, usize), String> {
    if page == 0 || ![25, 50, 100].contains(&page_size) || page_size > MAX_PAGE_SIZE {
        return Err(
            "INVALID_PAGINATION: Page must be positive and page size must be 25, 50, or 100."
                .into(),
        );
    }
    Ok((page, page_size))
}

fn page_of<T>(items: Vec<T>, page: usize, page_size: usize) -> Page<T> {
    let total = items.len();
    let start = page.saturating_sub(1).saturating_mul(page_size).min(total);
    let end = start.saturating_add(page_size).min(total);
    Page {
        items: items.into_iter().skip(start).take(end - start).collect(),
        total,
        page,
        page_size,
    }
}

fn optional_order<T: Ord>(
    left: Option<T>,
    right: Option<T>,
    descending: bool,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            let ordering = left.cmp(&right);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn normalized_isbn(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

#[tauri::command]
fn query_books(state: State<'_, AppState>, query: BookQuery) -> Result<Page<BookView>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    query_books_backend(&connection, query)
}

fn query_books_backend(
    connection: &Connection,
    query: BookQuery,
) -> Result<Page<BookView>, String> {
    let (page, page_size) = validate_pagination(query.page, query.page_size)?;
    if ![
        "title",
        "author",
        "authors",
        "publicationYear",
        "publication_year",
    ]
    .contains(&query.sort_by.as_str())
    {
        return Err("INVALID_SORT: Sort books by title, author, or publicationYear.".into());
    }
    let books: Vec<Book> = list_documents(connection, "books")?;
    let copies: Vec<CopyRecord> = list_documents(connection, "copies")?;
    let mut copies_by_book: HashMap<&str, Vec<&CopyRecord>> = HashMap::new();
    for copy in &copies {
        copies_by_book.entry(&copy.book_id).or_default().push(copy);
    }
    let needle = search_key(query.search.trim());
    let identifier_needle = identifier_key(query.search.trim());
    let isbn_needle = normalized_isbn(query.search.trim());
    let classification_needle = identifier_key(query.search.trim());
    let mut items: Vec<BookView> = Vec::new();
    for book in books {
        validate_schema(book.schema_version)?;
        let related: Vec<&CopyRecord> = copies_by_book
            .get(book.id.as_str())
            .cloned()
            .unwrap_or_default();
        if query.available_only && !related.iter().any(|copy| copy.status == "available") {
            continue;
        }
        let searchable = search_key(&book.title).contains(&needle)
            || book
                .authors
                .iter()
                .any(|author| search_key(author).contains(&needle))
            || book
                .ddc
                .as_deref()
                .is_some_and(|value| identifier_key(value).contains(&classification_needle))
            || book
                .lcc
                .as_deref()
                .is_some_and(|value| identifier_key(value).contains(&classification_needle))
            || book
                .tags
                .iter()
                .any(|value| search_key(value).contains(&needle))
            || book
                .identifiers
                .iter()
                .any(|value| identifier_key(value).contains(&identifier_needle))
            || book
                .language
                .as_deref()
                .is_some_and(|value| search_key(value).contains(&needle))
            || (!isbn_needle.is_empty()
                && normalized_isbn(book.isbn.as_deref().unwrap_or_default())
                    .contains(&isbn_needle))
            || related.iter().any(|copy| {
                identifier_key(&copy.copy_identifier).contains(&identifier_needle)
                    || search_key(copy.location.as_deref().unwrap_or_default()).contains(&needle)
            });
        if !needle.is_empty() && !searchable {
            continue;
        }
        items.push(BookView {
            total_copies: related.len(),
            available_copies: related
                .iter()
                .filter(|copy| copy.status == "available")
                .count(),
            book,
        });
    }
    items.sort_by(|left, right| {
        let order = match query.sort_by.as_str() {
            "author" | "authors" => optional_order(
                left.book.authors.first().map(|value| search_key(value)),
                right.book.authors.first().map(|value| search_key(value)),
                query.descending,
            ),
            "publicationYear" | "publication_year" => optional_order(
                left.book.publication_year,
                right.book.publication_year,
                query.descending,
            ),
            _ => optional_order(
                Some(search_key(&left.book.title)),
                Some(search_key(&right.book.title)),
                query.descending,
            ),
        };
        order.then_with(|| left.book.id.cmp(&right.book.id))
    });
    Ok(page_of(items, page, page_size))
}

#[tauri::command]
fn query_authors(
    state: State<'_, AppState>,
    query: AuthorQuery,
) -> Result<Page<AuthorBookView>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    query_authors_backend(&connection, query)
}

fn query_authors_backend(
    connection: &Connection,
    query: AuthorQuery,
) -> Result<Page<AuthorBookView>, String> {
    let (page, page_size) = validate_pagination(query.page, query.page_size)?;
    let books: Vec<Book> = list_documents(connection, "books")?;
    let copies: Vec<CopyRecord> = list_documents(connection, "copies")?;
    let mut copies_by_book: HashMap<&str, Vec<&CopyRecord>> = HashMap::new();
    for copy in &copies {
        copies_by_book.entry(&copy.book_id).or_default().push(copy);
    }
    let needle = search_key(query.search.trim());
    let mut items = Vec::new();
    for book in books {
        validate_schema(book.schema_version)?;
        let related = copies_by_book
            .get(book.id.as_str())
            .cloned()
            .unwrap_or_default();
        let view = BookView {
            total_copies: related.len(),
            available_copies: related
                .iter()
                .filter(|copy| copy.status == "available")
                .count(),
            book,
        };
        let mut seen = std::collections::HashSet::new();
        let authors: Vec<Option<String>> = if view.book.authors.is_empty() {
            vec![None]
        } else {
            view.book
                .authors
                .iter()
                .filter_map(|author| {
                    let key = identity_key(author);
                    seen.insert(key).then(|| Some(author.clone()))
                })
                .collect()
        };
        for author in authors {
            let matches = needle.is_empty()
                || author
                    .as_deref()
                    .is_some_and(|value| search_key(value).contains(&needle))
                || search_key(&view.book.title).contains(&needle);
            if matches {
                items.push(AuthorBookView {
                    author,
                    book: view.clone(),
                });
            }
        }
    }
    items.sort_by(|left, right| {
        optional_order(
            left.author.as_deref().map(search_key),
            right.author.as_deref().map(search_key),
            false,
        )
        .then_with(|| {
            optional_order(
                Some(search_key(&left.book.book.title)),
                Some(search_key(&right.book.book.title)),
                false,
            )
        })
        .then_with(|| left.author.cmp(&right.author))
        .then_with(|| left.book.book.id.cmp(&right.book.book.id))
    });
    Ok(page_of(items, page, page_size))
}

#[tauri::command]
fn query_patrons(state: State<'_, AppState>, query: PatronQuery) -> Result<Page<Patron>, String> {
    ensure_library_open(&state)?;
    let (page, page_size) = validate_pagination(query.page, query.page_size)?;
    if !["name", "identifier", "patronIdentifier"].contains(&query.sort_by.as_str()) {
        return Err("INVALID_SORT: Sort patrons by name or identifier.".into());
    }
    let connection = state.connection.lock().map_err(app_error)?;
    let needle = search_key(query.search.trim());
    let identifier_needle = identifier_key(query.search.trim());
    let mut items: Vec<Patron> = list_documents::<Patron>(&connection, "patrons")?
        .into_iter()
        .filter(|patron| {
            let status_matches = match query.status.as_deref() {
                Some("active") => patron.active,
                Some("inactive") => !patron.active,
                Some("") | None => true,
                _ => false,
            };
            status_matches
                && (needle.is_empty()
                    || search_key(&patron.name).contains(&needle)
                    || identifier_key(&patron.patron_identifier).contains(&identifier_needle))
        })
        .collect();
    items.sort_by(|left, right| {
        let order = match query.sort_by.as_str() {
            "identifier" | "patronIdentifier" => optional_order(
                Some(identifier_key(&left.patron_identifier)),
                Some(identifier_key(&right.patron_identifier)),
                query.descending,
            ),
            _ => optional_order(
                Some(search_key(&left.name)),
                Some(search_key(&right.name)),
                query.descending,
            ),
        };
        order.then_with(|| left.id.cmp(&right.id))
    });
    Ok(page_of(items, page, page_size))
}

#[tauri::command]
fn query_copies(state: State<'_, AppState>, query: CopyQuery) -> Result<Page<CopyView>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    query_copies_backend(&connection, query)
}

fn query_copies_backend(
    connection: &Connection,
    query: CopyQuery,
) -> Result<Page<CopyView>, String> {
    let (page, page_size) = validate_pagination(query.page, query.page_size)?;
    if !["location", "copyIdentifier", "copy_identifier", "status"]
        .contains(&query.sort_by.as_str())
    {
        return Err("INVALID_SORT: Sort copies by location, copyIdentifier, or status.".into());
    }
    let books: Vec<Book> = list_documents(connection, "books")?;
    let books_by_id: HashMap<&str, &Book> =
        books.iter().map(|book| (book.id.as_str(), book)).collect();
    let needle = search_key(query.search.trim());
    let identifier_needle = identifier_key(query.search.trim());
    let mut items: Vec<CopyView> = list_documents::<CopyRecord>(connection, "copies")?
        .into_iter()
        .filter(|copy| {
            query
                .book_id
                .as_ref()
                .is_none_or(|book_id| &copy.book_id == book_id)
        })
        .filter(|copy| {
            query
                .status
                .as_ref()
                .is_none_or(|status| &copy.status == status)
        })
        .map(|copy| {
            let book = books_by_id
                .get(copy.book_id.as_str())
                .ok_or_else(|| "DANGLING_REFERENCE: Copy refers to a missing book.".to_owned())?;
            let matches = needle.is_empty()
                || identifier_key(&copy.copy_identifier).contains(&identifier_needle)
                || search_key(copy.location.as_deref().unwrap_or_default()).contains(&needle)
                || search_key(&book.title).contains(&needle)
                || book
                    .authors
                    .iter()
                    .any(|author| search_key(author).contains(&needle));
            Ok(matches.then(|| CopyView {
                copy,
                book_title: book.title.clone(),
            }))
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect();
    items.sort_by(|left, right| {
        let order = match query.sort_by.as_str() {
            "copyIdentifier" | "copy_identifier" => optional_order(
                Some(identifier_key(&left.copy.copy_identifier)),
                Some(identifier_key(&right.copy.copy_identifier)),
                query.descending,
            ),
            "status" => optional_order(
                Some(left.copy.status.clone()),
                Some(right.copy.status.clone()),
                query.descending,
            ),
            _ => optional_order(
                left.copy.location.as_deref().map(search_key),
                right.copy.location.as_deref().map(search_key),
                query.descending,
            ),
        };
        order.then_with(|| left.copy.id.cmp(&right.copy.id))
    });
    Ok(page_of(items, page, page_size))
}

#[tauri::command]
fn query_loans(state: State<'_, AppState>, query: LoanQuery) -> Result<Page<LoanView>, String> {
    ensure_library_open(&state)?;
    let (page, page_size) = validate_pagination(query.page, query.page_size)?;
    if !["", "active", "completed"].contains(&query.status.as_str()) {
        return Err("INVALID_LOAN_FILTER: Status must be active or completed.".into());
    }
    if !["dueDate", "title", "bookTitle", "patronName"].contains(&query.sort_by.as_str()) {
        return Err("INVALID_SORT: Sort loans by dueDate, title, or patronName.".into());
    }
    let connection = state.connection.lock().map_err(app_error)?;
    let loans: Vec<Loan> = list_documents(&connection, "loans")?;
    let copies: Vec<CopyRecord> = list_documents(&connection, "copies")?;
    let books: Vec<Book> = list_documents(&connection, "books")?;
    let patrons: Vec<Patron> = list_documents(&connection, "patrons")?;
    let copies_by_id: HashMap<&str, &CopyRecord> =
        copies.iter().map(|copy| (copy.id.as_str(), copy)).collect();
    let books_by_id: HashMap<&str, &Book> =
        books.iter().map(|book| (book.id.as_str(), book)).collect();
    let patrons_by_id: HashMap<&str, &Patron> = patrons
        .iter()
        .map(|patron| (patron.id.as_str(), patron))
        .collect();
    let needle = search_key(query.search.trim());
    let identifier_needle = identifier_key(query.search.trim());
    let today = Local::now().date_naive();
    let mut items = Vec::new();
    for loan in loans {
        validate_schema(loan.schema_version)?;
        validate_loan(&loan)?;
        let active = loan.closed_at.is_none();
        if (query.status != "completed" && !active) || (query.status == "completed" && active) {
            continue;
        }
        if query
            .patron_id
            .as_ref()
            .is_some_and(|patron_id| patron_id != &loan.patron_id)
        {
            continue;
        }
        let overdue = is_overdue(&loan, today)?;
        if query.overdue_only && !overdue {
            continue;
        }
        let copy = copies_by_id
            .get(loan.copy_id.as_str())
            .copied()
            .ok_or_else(|| "DANGLING_REFERENCE: Loan refers to a missing copy.".to_owned())?;
        let book = books_by_id
            .get(copy.book_id.as_str())
            .copied()
            .ok_or_else(|| "DANGLING_REFERENCE: Copy refers to a missing book.".to_owned())?;
        let patron = patrons_by_id
            .get(loan.patron_id.as_str())
            .copied()
            .ok_or_else(|| "DANGLING_REFERENCE: Loan refers to a missing patron.".to_owned())?;
        if !needle.is_empty()
            && !search_key(&book.title).contains(&needle)
            && !search_key(&patron.name).contains(&needle)
            && !identifier_key(&copy.copy_identifier).contains(&identifier_needle)
        {
            continue;
        }
        items.push(LoanView {
            loan,
            copy_identifier: copy.copy_identifier.clone(),
            book_title: book.title.clone(),
            patron_name: patron.name.clone(),
            copy_status: copy.status.clone(),
            overdue,
        });
    }
    items.sort_by(|left, right| {
        let order = match query.sort_by.as_str() {
            "title" | "bookTitle" => optional_order(
                Some(search_key(&left.book_title)),
                Some(search_key(&right.book_title)),
                query.descending,
            ),
            "patronName" => optional_order(
                Some(search_key(&left.patron_name)),
                Some(search_key(&right.patron_name)),
                query.descending,
            ),
            _ => optional_order(
                Some(left.loan.due_date.clone()),
                Some(right.loan.due_date.clone()),
                query.descending,
            ),
        };
        order.then_with(|| left.loan.id.cmp(&right.loan.id))
    });
    Ok(page_of(items, page, page_size))
}

fn managed_cover_path(data_dir: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative_path);
    if path.components().count() != 2
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || path
            .components()
            .next()
            .map(|component| component.as_os_str())
            != Some("covers".as_ref())
    {
        return Err("INVALID_COVER_PATH: Cover path is outside managed storage.".into());
    }
    let root = data_dir.canonicalize().map_err(app_error)?;
    let covers = root.join("covers");
    let metadata = fs::symlink_metadata(&covers).map_err(app_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("INVALID_COVER_PATH: Managed covers directory is not a safe directory.".into());
    }
    let covers = covers.canonicalize().map_err(app_error)?;
    if !covers.starts_with(&root) {
        return Err(
            "INVALID_COVER_PATH: Managed covers directory escapes the selected library.".into(),
        );
    }
    let filename = path
        .file_name()
        .ok_or_else(|| "INVALID_COVER_PATH: Missing cover filename.".to_owned())?;
    let target = covers.join(filename);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err("INVALID_COVER_PATH: Cover asset cannot be a symlink.".into());
        }
    }
    Ok(target)
}

fn remove_managed_cover(data_dir: &Path, relative_path: &str) -> Result<(), String> {
    let path = managed_cover_path(data_dir, relative_path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(app_error(error)),
    }
}

fn cleanup_cover_if_unreferenced(
    state: &AppState,
    library_dir: &Path,
    relative_path: &str,
) -> Result<(), String> {
    let connection = state.connection.lock().map_err(app_error)?;
    let books: Vec<Book> = list_documents(&connection, "books")?;
    if books.iter().any(|book| {
        book.cover_image
            .as_ref()
            .is_some_and(|cover| cover.path == relative_path)
    }) {
        return Ok(());
    }
    drop(connection);
    remove_managed_cover(library_dir, relative_path).map_err(|error| {
        format!("COVER_COMMITTED_CLEANUP_FAILED: Cover reference was committed, but old asset cleanup failed: {error}")
    })
}

fn reconcile_owned_covers(connection: &Connection, library_dir: &Path) -> Result<(), String> {
    let books: Vec<Book> = list_documents(connection, "books")?;
    let referenced: std::collections::HashSet<String> = books
        .iter()
        .filter_map(|book| book.cover_image.as_ref().map(|cover| cover.path.clone()))
        .collect();
    let covers_dir = library_dir.join("covers");
    let metadata = fs::symlink_metadata(&covers_dir).map_err(app_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("INVALID_COVER_PATH: Managed covers directory is not a safe directory.".into());
    }
    for entry in fs::read_dir(covers_dir).map_err(app_error)? {
        let entry = entry.map_err(app_error)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let path = Path::new(name);
        let extension = path.extension().and_then(|extension| extension.to_str());
        let stem = path.file_stem().and_then(|stem| stem.to_str());
        if !matches!(extension, Some("jpg" | "png"))
            || !stem.is_some_and(|stem| {
                Uuid::parse_str(stem).is_ok()
                    || stem
                        .rsplit_once("--")
                        .is_some_and(|(_, id)| Uuid::parse_str(id).is_ok())
            })
        {
            continue;
        }
        let relative = format!("covers/{name}");
        if referenced.contains(&relative) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(app_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        fs::remove_file(entry.path()).map_err(app_error)?;
    }
    Ok(())
}

#[tauri::command(async)]
fn set_cover(
    state: State<'_, AppState>,
    book_id: String,
    source_path: String,
    operation_id: Option<String>,
) -> Result<Book, String> {
    set_cover_internal(&state, book_id, source_path, operation_id)
}

fn set_cover_internal(
    state: &AppState,
    book_id: String,
    source_path: String,
    operation_id: Option<String>,
) -> Result<Book, String> {
    ensure_library_open(state)?;
    let source = PathBuf::from(source_path);
    let metadata = fs::metadata(&source).map_err(app_error)?;
    if !metadata.is_file() || metadata.len() > MAX_COVER_BYTES {
        return Err("INVALID_IMAGE_SIZE: Select a JPEG or PNG file smaller than 10 MB.".into());
    }
    let bytes = fs::read(&source).map_err(app_error)?;
    install_cover_bytes(state, book_id, bytes, operation_id)
}

fn update_cover_operation(
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

fn cover_operation_cancelled(state: &AppState, operation_id: Option<&str>) -> Result<(), String> {
    if operation_id.is_some_and(|id| {
        state
            .cancelled_operations
            .lock()
            .map(|cancelled| cancelled.contains(id))
            .unwrap_or(false)
    }) {
        Err("OPERATION_CANCELLED: Cover replacement was cancelled before saving.".into())
    } else {
        Ok(())
    }
}

fn install_cover_bytes(
    state: &AppState,
    book_id: String,
    bytes: Vec<u8>,
    operation_id: Option<String>,
) -> Result<Book, String> {
    ensure_library_open(state)?;
    update_cover_operation(state, operation_id.as_deref(), "validating", 0, 1, true)?;
    let (extension, media_type) = validate_cover_image(&bytes)?;
    cover_operation_cancelled(state, operation_id.as_deref())?;
    update_cover_operation(state, operation_id.as_deref(), "saving", 0, 1, true)?;
    let library_dir = current_library_dir(state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let current_book = get_book(&connection, &book_id)?;
    let previous = current_book.cover_image.clone();
    let relative_path = cover_relative_path(&current_book.title, &book_id, extension);
    let destination = managed_cover_path(&library_dir, &relative_path)?;
    let replaces_same_path = previous
        .as_ref()
        .is_some_and(|cover| cover.path == relative_path);
    let staged_path = if replaces_same_path {
        library_dir
            .join("covers")
            .join(format!(".cover-{}.partial", Uuid::new_v4()))
    } else {
        destination.clone()
    };
    if let Err(error) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged_path)
        .and_then(|mut file| {
            std::io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()
        })
    {
        let _ = fs::remove_file(&staged_path);
        return Err(app_error(error));
    }

    if let Err(error) = cover_operation_cancelled(state, operation_id.as_deref()) {
        let _ = fs::remove_file(&staged_path);
        return Err(error);
    }
    let mut book = current_book;
    book.cover_image = Some(CoverImage {
        path: relative_path.clone(),
        media_type: media_type.into(),
    });
    book.updated_at = now_string();
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    if let Err(error) = save_document(&tx, "books", &book_id, &book) {
        let _ = fs::remove_file(&staged_path);
        return Err(error);
    }
    update_cover_operation(state, operation_id.as_deref(), "committing", 0, 1, false)?;
    if let Err(error) = tx.commit().map_err(app_error) {
        let _ = fs::remove_file(&staged_path);
        return Err(error);
    }
    drop(connection);

    if replaces_same_path {
        let backup = library_dir
            .join("covers")
            .join(format!(".cover-{}.previous", Uuid::new_v4()));
        if let Err(error) = fs::rename(&destination, &backup) {
            let _ = fs::remove_file(&staged_path);
            return Err(format!(
                "COVER_COMMITTED_CLEANUP_FAILED: Cover metadata was committed, but the previous asset could not be staged: {error}"
            ));
        }
        if let Err(error) = fs::rename(&staged_path, &destination) {
            let _ = fs::rename(&backup, &destination);
            let _ = fs::remove_file(&staged_path);
            return Err(format!(
                "COVER_COMMITTED_CLEANUP_FAILED: Cover metadata was committed, but the managed asset could not be replaced: {error}"
            ));
        }
        fs::remove_file(&backup).map_err(|error| {
            format!("COVER_COMMITTED_CLEANUP_FAILED: Cover was replaced, but the old asset cleanup failed: {error}")
        })?;
    }
    if let Some(old_cover) = previous {
        if old_cover.path != relative_path {
            cleanup_cover_if_unreferenced(state, &library_dir, &old_cover.path)?;
        }
    }
    update_cover_operation(state, operation_id.as_deref(), "completed", 1, 1, false)?;
    Ok(book)
}

#[tauri::command(async)]
fn set_cover_from_clipboard(
    state: State<'_, AppState>,
    book_id: String,
    operation_id: Option<String>,
) -> Result<Book, String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| format!("CLIPBOARD_UNAVAILABLE: Cannot access the clipboard: {error}"))?;
    let image = clipboard.get_image().map_err(|error| {
        format!("CLIPBOARD_IMAGE_MISSING: Copy a JPEG or PNG image first: {error}")
    })?;
    if image.width > 8_000 || image.height > 8_000 {
        return Err("INVALID_IMAGE: Clipboard image dimensions exceed 8,000 pixels.".into());
    }
    let expected = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "INVALID_IMAGE: Clipboard image dimensions are invalid.".to_owned())?;
    if expected != image.bytes.len() || expected > 128 * 1024 * 1024 {
        return Err("INVALID_IMAGE: Clipboard image allocation exceeds 128 MiB.".into());
    }
    let rgba = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.into_owned(),
    )
    .ok_or_else(|| "INVALID_IMAGE: Clipboard image bytes are invalid.".to_owned())?;
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut encoded, ImageFormat::Png)
        .map_err(app_error)?;
    install_cover_bytes(&state, book_id, encoded.into_inner(), operation_id)
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224)
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(ip));
            }
            let segments = ip.segments();
            (segments[0] & 0xe000) == 0x2000 && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

fn public_socket_for(url: &reqwest::Url) -> Result<(String, SocketAddr), String> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err("COVER_URL_CREDENTIALS: Cover URLs cannot contain credentials.".into());
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err("COVER_URL_SCHEME: Use an HTTP or HTTPS cover URL.".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "COVER_URL_INVALID: Cover URL requires a host.".to_owned())?
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "COVER_URL_INVALID: Cover URL has no usable port.".to_owned())?;
    let addresses: Vec<SocketAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("COVER_URL_DNS: Cannot resolve cover URL host: {error}"))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err("COVER_URL_BLOCKED: Cover URL must resolve only to public addresses.".into());
    }
    Ok((host, addresses[0]))
}

fn download_cover(
    state: &AppState,
    source_url: &str,
    operation_id: Option<&str>,
) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    let mut url = reqwest::Url::parse(source_url)
        .map_err(|error| format!("COVER_URL_INVALID: Enter a valid HTTP(S) URL: {error}"))?;
    for redirect_count in 0..=MAX_COVER_REDIRECTS {
        cover_operation_cancelled(state, operation_id)?;
        let remaining = COVER_DOWNLOAD_TIMEOUT
            .checked_sub(started.elapsed())
            .ok_or_else(|| "COVER_URL_TIMEOUT: Cover download exceeded 30 seconds.".to_owned())?;
        let (host, address) = public_socket_for(&url)?;
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(remaining)
            .no_proxy()
            .resolve(&host, address)
            .build()
            .map_err(app_error)?;
        let mut response = client
            .get(url.clone())
            .send()
            .map_err(|error| format!("COVER_URL_DOWNLOAD: Cover download failed: {error}"))?;
        if response.status().is_redirection() {
            if redirect_count == MAX_COVER_REDIRECTS {
                return Err("COVER_URL_REDIRECTS: Cover URL exceeded five redirects.".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| "COVER_URL_REDIRECTS: Redirect has no location.".to_owned())?
                .to_str()
                .map_err(|_| "COVER_URL_REDIRECTS: Redirect location is invalid.".to_owned())?;
            url = url
                .join(location)
                .map_err(|error| format!("COVER_URL_REDIRECTS: Invalid redirect: {error}"))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "COVER_URL_STATUS: Cover server returned HTTP {}.",
                response.status()
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_COVER_BYTES)
        {
            return Err("INVALID_IMAGE_SIZE: Cover images cannot exceed 10 MiB.".into());
        }
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            cover_operation_cancelled(state, operation_id)?;
            if started.elapsed() >= COVER_DOWNLOAD_TIMEOUT {
                return Err("COVER_URL_TIMEOUT: Cover download exceeded 30 seconds.".into());
            }
            let count = response
                .read(&mut buffer)
                .map_err(|error| format!("COVER_URL_DOWNLOAD: Cover download failed: {error}"))?;
            if count == 0 {
                return Ok(bytes);
            }
            if bytes.len() + count > MAX_COVER_BYTES as usize {
                return Err("INVALID_IMAGE_SIZE: Cover images cannot exceed 10 MiB.".into());
            }
            bytes.extend_from_slice(&buffer[..count]);
            update_cover_operation(
                state,
                operation_id,
                "downloading",
                bytes.len(),
                response.content_length().unwrap_or(0) as usize,
                true,
            )?;
        }
    }
    Err("COVER_URL_REDIRECTS: Cover URL exceeded five redirects.".into())
}

#[tauri::command(async)]
fn set_cover_from_url(
    state: State<'_, AppState>,
    book_id: String,
    url: String,
    operation_id: Option<String>,
) -> Result<Book, String> {
    ensure_library_open(&state)?;
    update_cover_operation(&state, operation_id.as_deref(), "downloading", 0, 0, true)?;
    let bytes = download_cover(&state, url.trim(), operation_id.as_deref())?;
    install_cover_bytes(&state, book_id, bytes, operation_id)
}

#[tauri::command]
fn remove_cover(state: State<'_, AppState>, book_id: String) -> Result<Book, String> {
    ensure_library_open(&state)?;
    let library_dir = current_library_dir(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let mut book = get_book(&connection, &book_id)?;
    let previous = book.cover_image.take();
    book.updated_at = now_string();
    let tx = connection.unchecked_transaction().map_err(app_error)?;
    save_document(&tx, "books", &book_id, &book)?;
    tx.commit().map_err(app_error)?;
    drop(connection);
    if let Some(cover) = previous {
        cleanup_cover_if_unreferenced(&state, &library_dir, &cover.path)?;
    }
    Ok(book)
}

#[tauri::command]
fn cover_data(state: State<'_, AppState>, book_id: String) -> Result<Option<String>, String> {
    ensure_library_open(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    let book = get_book(&connection, &book_id)?;
    let Some(cover) = book.cover_image else {
        return Ok(None);
    };
    let path = managed_cover_path(&current_library_dir(&state)?, &cover.path)?;
    let bytes = fs::read(path).map_err(app_error)?;
    Ok(Some(format!(
        "data:{};base64,{}",
        cover.media_type,
        STANDARD.encode(bytes)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tempfile::tempdir;

    pub(crate) fn isolated_state(root: &Path) -> AppState {
        AppState {
            connection: Mutex::new(Connection::open_in_memory().unwrap()),
            database_path: Mutex::new(PathBuf::new()),
            library_dir: Mutex::new(root.join("unselected")),
            library_open: Mutex::new(false),
            library_lock: Mutex::new(None),
            app_settings_path: root.join("application-settings.json"),
            app_language: Mutex::new("fr".into()),
            books_view: Mutex::new(default_books_view()),
            default_library_dir: root.join("default-library"),
            testing_mode: true,
            csv_preview: Mutex::new(None),
            metadata_lookups: Mutex::new(HashMap::new()),
            cancelled_operations: Mutex::new(Default::default()),
            operation_status: Mutex::new(Default::default()),
            startup_error: Mutex::new(None),
        }
    }

    pub(crate) fn create_fixture_library(root: &Path) {
        fs::create_dir_all(root.join("covers")).unwrap();
        let connection = open_database(&root.join("library.sqlite3")).unwrap();
        seed_lending_records(&connection);
    }

    #[test]
    fn unicode_full_case_folding_preserves_duplicate_accents_and_expands_nontrivial_pairs() {
        for (left, right) in [
            ("Straße", "STRASSE"),
            ("ΟΣ", "ος"),
            ("ﬃ", "FFI"),
            ("ᾲ", "ὰι"),
            ("և", "ԵՒ"),
        ] {
            assert_eq!(identity_key(left), identity_key(right), "{left} / {right}");
            assert_eq!(search_key(left), search_key(right), "{left} / {right}");
        }
        assert_ne!(identity_key("café"), identity_key("cafe"));
        assert_eq!(identity_key("café"), identity_key("cafe\u{301}"));
        assert_eq!(search_key("café"), search_key("CAFE"));
        assert_eq!(
            search_key("  Français \t Bibliothèque "),
            "francais bibliotheque"
        );
        assert_eq!(identity_key("A  Book"), identity_key("a book"));
        assert_ne!(identifier_key("É0001"), identifier_key("e0001"));
    }

    #[test]
    fn copy_picker_searches_book_title_authors_identifier_location_and_bounds_pages() {
        let connection = open_database(Path::new(":memory:")).unwrap();
        let (mut book, copy, _) = seed_lending_records(&connection);
        book.title = "Straße Café".into();
        book.authors = vec!["Σίσυφος Writer".into()];
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &book.id, &book).unwrap();
        for index in 0..30 {
            let mut extra = copy.clone();
            extra.id = format!("picker-{index:03}");
            extra.copy_identifier = format!("PICK-{index:03}");
            save_document(&tx, "copies", &extra.id, &extra).unwrap();
        }
        tx.commit().unwrap();
        let query = |search: &str, page| CopyQuery {
            search: search.into(),
            book_id: Some(book.id.clone()),
            status: Some("available".into()),
            page,
            page_size: 25,
            sort_by: "copyIdentifier".into(),
            descending: false,
        };
        for search in ["STRASSE CAFE", "ΣΊΣΥΦΟΣ", "Room A", "000045"] {
            let results = query_copies_backend(&connection, query(search, 1)).unwrap();
            assert!(results.total > 0, "{search}");
            assert!(results.items.len() <= 25);
            assert!(results
                .items
                .iter()
                .all(|view| view.book_title == book.title));
            let serialized = serde_json::to_value(&results.items[0]).unwrap();
            assert_eq!(serialized["bookTitle"], book.title);
            assert!(serialized.get("copyIdentifier").is_some());
            assert!(serialized.get("copy").is_none());
        }
        let first = query_copies_backend(&connection, query("cafe", 1)).unwrap();
        let second = query_copies_backend(&connection, query("cafe", 2)).unwrap();
        assert_eq!(
            (
                first.total,
                first.items.len(),
                second.total,
                second.items.len()
            ),
            (31, 25, 31, 6)
        );
        assert!(!first.items.iter().any(|left| second
            .items
            .iter()
            .any(|right| left.copy.id == right.copy.id)));
        assert!(query_copies_backend(&connection, query("cafe", 3))
            .unwrap()
            .items
            .is_empty());
    }

    #[test]
    fn portable_library_move_reopen_settings_and_failed_locked_switch_preserve_selection() {
        let dir = tempfile::Builder::new()
            .prefix(".backend-library-")
            .tempdir_in(".")
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let original = root.join("original");
        let moved = root.join("moved");
        let other = root.join("other");
        create_fixture_library(&original);
        create_fixture_library(&other);
        let state = isolated_state(&root);
        open_library_internal(&state, &original).unwrap();
        let settings = read_application_settings(&state.app_settings_path).unwrap();
        assert_eq!(settings.language, "fr");
        assert_eq!(settings.books_view, "list");
        assert_eq!(settings.library_path.as_deref(), original.to_str());
        drop(state);
        fs::rename(&original, &moved).unwrap();
        let state = isolated_state(&root);
        open_library_internal(&state, &moved).unwrap();
        let before = fs::read(&state.app_settings_path).unwrap();
        let (_other_connection, _other_lock) = open_existing_library(&other).unwrap();
        assert!(open_library_internal(&state, &other)
            .unwrap_err()
            .starts_with("LIBRARY_LOCKED:"));
        assert!(open_library_internal(&state, &root.join("missing")).is_err());
        assert_eq!(*state.library_dir.lock().unwrap(), moved);
        assert_eq!(fs::read(&state.app_settings_path).unwrap(), before);
        assert_eq!(
            list_documents::<Book>(&state.connection.lock().unwrap(), "books")
                .unwrap()
                .len(),
            1
        );
        assert!(!root.join("missing").exists());
        drop(state);
        let settings = read_application_settings(&root.join("application-settings.json")).unwrap();
        let (connection, _lock) =
            open_existing_library(Path::new(settings.library_path.as_ref().unwrap())).unwrap();
        assert_eq!(settings.language, "fr");
        assert_eq!(settings.books_view, "list");
        assert_eq!(
            list_documents::<CopyRecord>(&connection, "copies")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn application_books_view_defaults_to_list_and_persists_grid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("application-settings.json");
        fs::write(&path, br#"{"language":"en","libraryPath":null}"#).unwrap();
        assert_eq!(read_application_settings(&path).unwrap().books_view, "list");
        write_application_settings(&path, "en", None, "grid").unwrap();
        assert_eq!(read_application_settings(&path).unwrap().books_view, "grid");
        assert!(write_application_settings(&path, "en", None, "table").is_err());
    }

    #[test]
    fn authors_query_orders_deduplicates_and_repeats_multi_author_books() {
        let connection = open_database(Path::new(":memory:")).unwrap();
        let (mut book, _, _) = seed_lending_records(&connection);
        book.authors = vec![
            "Zoë Writer".into(),
            "Ana Author".into(),
            "ana author".into(),
        ];
        let mut authorless = book.clone();
        authorless.id = "book-authorless".into();
        authorless.title = "No Author".into();
        authorless.authors.clear();
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &book.id, &book).unwrap();
        save_document(&tx, "books", &authorless.id, &authorless).unwrap();
        tx.commit().unwrap();

        let result = query_authors_backend(
            &connection,
            AuthorQuery {
                search: String::new(),
                page: 1,
                page_size: 50,
            },
        )
        .unwrap();
        assert_eq!(result.total, 3);
        assert_eq!(result.items[0].author.as_deref(), Some("Ana Author"));
        assert_eq!(result.items[1].author.as_deref(), Some("Zoë Writer"));
        assert_eq!(result.items[2].author, None);

        let filtered = query_authors_backend(
            &connection,
            AuthorQuery {
                search: "zoe".into(),
                page: 1,
                page_size: 25,
            },
        )
        .unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.items[0].book.book.id, book.id);
    }

    #[test]
    fn cover_replacement_valid_corrupt_oversized_and_failed_write_preserve_old_asset() {
        let dir = tempfile::Builder::new()
            .prefix(".backend-covers-")
            .tempdir_in(".")
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let library = root.join("library");
        create_fixture_library(&library);
        let state = isolated_state(&root);
        open_library_internal(&state, &library).unwrap();
        let book: Book = list_documents(&state.connection.lock().unwrap(), "books")
            .unwrap()
            .remove(0);
        let source = root.join("source.png");
        image::DynamicImage::new_rgb8(2, 2).save(&source).unwrap();
        let first = set_cover_internal(&state, book.id.clone(), source.display().to_string(), None)
            .unwrap();
        let first_path = first.cover_image.unwrap().path;
        assert_eq!(
            first_path,
            format!("covers/{}--{}.png", book.title, book.id)
        );
        let previous_bytes = fs::read(library.join(&first_path)).unwrap();
        let corrupt = root.join("corrupt.png");
        fs::write(&corrupt, b"not an image").unwrap();
        let oversized = root.join("oversized.png");
        let file = fs::File::create(&oversized).unwrap();
        file.set_len(MAX_COVER_BYTES + 1).unwrap();
        for invalid in [&corrupt, &oversized] {
            assert!(set_cover_internal(
                &state,
                book.id.clone(),
                invalid.display().to_string(),
                None
            )
            .is_err());
            let current = get_book(&state.connection.lock().unwrap(), &book.id).unwrap();
            assert_eq!(current.cover_image.unwrap().path, first_path);
            assert_eq!(fs::read(library.join(&first_path)).unwrap(), previous_bytes);
        }
        state
            .connection
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_cover BEFORE UPDATE ON documents WHEN NEW.collection='books'
             BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;",
            )
            .unwrap();
        assert!(
            set_cover_internal(&state, book.id.clone(), source.display().to_string(), None)
                .is_err()
        );
        assert_eq!(fs::read_dir(library.join("covers")).unwrap().count(), 1);
        assert_eq!(
            get_book(&state.connection.lock().unwrap(), &book.id)
                .unwrap()
                .cover_image
                .unwrap()
                .path,
            first_path
        );
        state
            .connection
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_cover;")
            .unwrap();
        let replacement =
            set_cover_internal(&state, book.id, source.display().to_string(), None).unwrap();
        assert_eq!(replacement.cover_image.unwrap().path, first_path);
        assert!(library.join(first_path).exists());
        assert_eq!(fs::read_dir(library.join("covers")).unwrap().count(), 1);
    }

    #[test]
    fn cover_names_are_portable_unique_and_public_url_targets_are_required() {
        assert_eq!(
            cover_relative_path(" CON:  Rust / Guide. ", "book-id", "png"),
            "covers/CON_ Rust _ Guide--book-id.png"
        );
        assert_eq!(sanitized_cover_title("CON"), "_CON");
        assert_eq!(
            cover_relative_path("***", "book-id", "jpg"),
            "covers/book--book-id.jpg"
        );
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.1.1".parse().unwrap()));
        assert!(!is_public_ip("2001:db8::1".parse().unwrap()));
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));

        let blocked = reqwest::Url::parse("http://127.0.0.1/cover.png").unwrap();
        assert!(public_socket_for(&blocked).is_err());
        let credentials = reqwest::Url::parse("https://user:secret@example.com/cover.png").unwrap();
        assert!(public_socket_for(&credentials).is_err());
        let unsupported = reqwest::Url::parse("file:///tmp/cover.png").unwrap();
        assert!(public_socket_for(&unsupported).is_err());
    }

    fn seed_lending_records(connection: &Connection) -> (Book, CopyRecord, Patron) {
        let now = now_string();
        let book = Book {
            schema_version: SCHEMA_VERSION,
            id: "book-flow".into(),
            title: "Français for Rust".into(),
            isbn: Some("000-123".into()),
            authors: vec!["Élodie Reader".into()],
            publisher: None,
            publication_year: Some(2024),
            genre: None,
            ddc: Some("005.133".into()),
            lcc: Some("QA76.73.R87".into()),
            tags: vec![],
            identifiers: vec![],
            language: None,
            pages: None,
            notes: None,
            cover_image: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            extra: serde_json::Map::new(),
        };
        let copy = CopyRecord {
            schema_version: SCHEMA_VERSION,
            id: "copy-flow".into(),
            book_id: book.id.clone(),
            copy_identifier: "000045".into(),
            status: "available".into(),
            location: Some("Room A / Shelf 3".into()),
            acquisition_date: None,
            notes: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let patron = Patron {
            schema_version: SCHEMA_VERSION,
            id: "patron-flow".into(),
            patron_identifier: "000012".into(),
            name: "François Reader".into(),
            contact_information: None,
            notes: None,
            active: true,
            created_at: now.clone(),
            updated_at: now,
        };
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &book.id, &book).unwrap();
        save_document(&tx, "copies", &copy.id, &copy).unwrap();
        save_document(&tx, "patrons", &patron.id, &patron).unwrap();
        tx.commit().unwrap();
        (book, copy, patron)
    }

    #[test]
    fn dialog_configuration_uses_v2_capabilities() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        for native_icon in ["icons/icon.png", "icons/icon.ico", "icons/icon.icns"] {
            assert!(config["bundle"]["icon"]
                .as_array()
                .unwrap()
                .iter()
                .any(|icon| icon == native_icon));
            assert!(Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(native_icon)
                .is_file());
        }
        let dialog_config = config["plugins"]["dialog"].clone();
        serde_json::from_value::<()>(dialog_config).unwrap();

        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        for permission in [
            "dialog:allow-open",
            "dialog:allow-save",
            "core:window:allow-close",
            "core:window:allow-set-title",
        ] {
            assert!(permissions.iter().any(|value| value == permission));
        }
    }

    #[test]
    fn sqlite_document_store_enforces_unique_ids() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        let first = CopyRecord {
            schema_version: SCHEMA_VERSION,
            id: "copy-1".into(),
            book_id: "book-1".into(),
            copy_identifier: "BAR-1".into(),
            status: "available".into(),
            location: None,
            acquisition_date: None,
            notes: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        save_document(&tx, "copies", &first.id, &first).unwrap();
        let mut duplicate = first.clone();
        duplicate.id = "copy-2".into();
        assert!(save_document(&tx, "copies", &duplicate.id, &duplicate).is_err());
        tx.rollback().unwrap();
    }

    #[test]
    fn version_one_migration_preserves_documents_and_derives_loan_closure() {
        let dir = tempdir().unwrap();
        let database_path = dir.path().join("library.sqlite3");
        let legacy = Connection::open(&database_path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE documents (
                 collection TEXT NOT NULL,
                 id TEXT NOT NULL,
                 body TEXT NOT NULL CHECK (json_valid(body)),
                 PRIMARY KEY (collection, id)
             );
             PRAGMA user_version = 1;",
            )
            .unwrap();
        legacy.execute(
            "INSERT INTO documents(collection,id,body) VALUES ('books','book-old',?1)",
            [r#"{"schemaVersion":1,"id":"book-old","title":"Old book","authors":[],"createdAt":"2024-01-01T00:00:00Z","updatedAt":"2024-01-01T00:00:00Z","legacyField":{"keep":true}}"#],
        ).unwrap();
        legacy.execute(
            "INSERT INTO documents(collection,id,body) VALUES ('loans','returned',?1)",
            [r#"{"schemaVersion":1,"id":"returned","copyId":"copy-1","patronId":"patron-1","checkoutDate":"2024-01-01","dueDate":"2024-01-22","returnDate":"2024-01-10"}"#],
        ).unwrap();
        legacy.execute(
            "INSERT INTO documents(collection,id,body) VALUES ('loans','active',?1)",
            [r#"{"schemaVersion":1,"id":"active","copyId":"copy-2","patronId":"patron-1","checkoutDate":"2024-01-01","dueDate":"2024-01-22","returnDate":null}"#],
        ).unwrap();
        drop(legacy);

        let connection = open_database(&database_path).unwrap();
        let old_book: Book = read_document(&connection, "books", "book-old")
            .unwrap()
            .unwrap();
        assert_eq!(old_book.ddc, None);
        assert_eq!(old_book.lcc, None);
        assert!(old_book.tags.is_empty());
        assert!(old_book.identifiers.is_empty());
        assert_eq!(old_book.language, None);
        assert_eq!(old_book.pages, None);
        assert_eq!(old_book.extra["legacyField"]["keep"], true);
        let returned: Loan = read_document(&connection, "loans", "returned")
            .unwrap()
            .unwrap();
        assert_eq!(returned.closed_at.as_deref(), Some("2024-01-10"));
        assert_eq!(returned.closure_reason.as_deref(), Some("returned"));
        let active: Loan = read_document(&connection, "loans", "active")
            .unwrap()
            .unwrap();
        assert_eq!(active.closed_at, None);
        assert_eq!(active.closure_reason, None);
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &old_book.id, &old_book).unwrap();
        tx.commit().unwrap();
        let preserved: serde_json::Value = read_document(&connection, "books", "book-old")
            .unwrap()
            .map(|book: Book| serde_json::to_value(book).unwrap())
            .unwrap();
        assert_eq!(preserved["legacyField"]["keep"], true);
        assert!(database_path
            .with_file_name("library.sqlite3.pre-migration-v1.bak")
            .is_file());
    }

    #[test]
    fn lost_checkout_resolution_recovery_and_withdrawal_follow_valid_transitions() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.sqlite3")).unwrap();
        let (_book, copy, patron) = seed_lending_records(&connection);
        let checkout_date = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        let loan =
            checkout_in_transaction(&tx, &copy.id, &patron.id, Some("2026-10-08"), checkout_date)
                .unwrap();
        tx.commit().unwrap();

        let mark_lost = connection.unchecked_transaction().unwrap();
        change_copy_status_in_transaction(&mark_lost, &copy.id, "lost").unwrap();
        mark_lost.commit().unwrap();
        let active: Loan = read_document(&connection, "loans", &loan.id)
            .unwrap()
            .unwrap();
        assert!(is_overdue(&active, NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()).unwrap());

        let invalid_recovery = connection.unchecked_transaction().unwrap();
        assert!(
            change_copy_status_in_transaction(&invalid_recovery, &copy.id, "available").is_err()
        );
        drop(invalid_recovery);
        let recover = connection.unchecked_transaction().unwrap();
        change_copy_status_in_transaction(&recover, &copy.id, "checked_out").unwrap();
        recover.commit().unwrap();
        let returned = connection.unchecked_transaction().unwrap();
        return_in_transaction(
            &returned,
            &loan.id,
            NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        )
        .unwrap();
        returned.commit().unwrap();
        let available: CopyRecord = read_document(&connection, "copies", &copy.id)
            .unwrap()
            .unwrap();
        assert_eq!(available.status, "available");

        let second = connection.unchecked_transaction().unwrap();
        let second_loan = checkout_in_transaction(
            &second,
            &copy.id,
            &patron.id,
            Some("2026-10-22"),
            checkout_date,
        )
        .unwrap();
        second.commit().unwrap();
        let lose_again = connection.unchecked_transaction().unwrap();
        change_copy_status_in_transaction(&lose_again, &copy.id, "lost").unwrap();
        lose_again.commit().unwrap();
        let resolve = connection.unchecked_transaction().unwrap();
        let resolved = resolve_lost_loan_in_transaction(
            &resolve,
            &second_loan.id,
            "Replacement will not be returned",
            NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        )
        .unwrap();
        resolve.commit().unwrap();
        assert_eq!(resolved.return_date, None);
        assert_eq!(resolved.closure_reason.as_deref(), Some("lost_resolved"));
        assert_eq!(
            resolved.resolution_note.as_deref(),
            Some("Replacement will not be returned")
        );
        let withdraw = connection.unchecked_transaction().unwrap();
        change_copy_status_in_transaction(&withdraw, &copy.id, "withdrawn").unwrap();
        withdraw.commit().unwrap();
        let reinstate = connection.unchecked_transaction().unwrap();
        assert!(change_copy_status_in_transaction(&reinstate, &copy.id, "available").is_err());
    }

    #[test]
    #[ignore = "large deterministic performance fixture; run with --ignored --nocapture"]
    fn backend_fixture_query_metrics_10k_books_20k_copies_5k_patrons_50k_loans() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("covers")).unwrap();
        let connection = open_database(&root.join("library.sqlite3")).unwrap();
        let image = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            1,
            1,
            image::Rgba([180, 120, 80, 255]),
        ));
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png).unwrap();
        let png = png.into_inner();
        let tx = connection.unchecked_transaction().unwrap();
        for index in 0..10_000 {
            let now = "2026-01-01T00:00:00Z".to_owned();
            let cover_image = if index < 1_000 {
                let path = format!("covers/fixture-{index:04}.png");
                fs::write(root.join(&path), &png).unwrap();
                Some(CoverImage {
                    path,
                    media_type: "image/png".into(),
                })
            } else {
                None
            };
            let book = Book {
                schema_version: SCHEMA_VERSION,
                id: format!("book-{index:05}"),
                title: format!("Book {index:05}"),
                isbn: Some(format!("{:013}", index)),
                authors: vec![format!("Author {:04}", index % 500)],
                publisher: None,
                publication_year: Some(1900 + (index % 126)),
                genre: None,
                ddc: Some(format!("{:03}.{:03}", index % 999, index % 1000)),
                lcc: Some(format!("QA{index:05}")),
                tags: vec![],
                identifiers: vec![],
                language: None,
                pages: None,
                notes: None,
                cover_image,
                created_at: now.clone(),
                updated_at: now,
                extra: serde_json::Map::new(),
            };
            save_document(&tx, "books", &book.id, &book).unwrap();
        }
        for index in 0..20_000 {
            let now = "2026-01-01T00:00:00Z".to_owned();
            let copy = CopyRecord {
                schema_version: SCHEMA_VERSION,
                id: format!("copy-{index:05}"),
                book_id: format!("book-{:05}", index / 2),
                copy_identifier: format!("CP{index:05}"),
                status: if index < 1_000 {
                    "checked_out"
                } else {
                    "available"
                }
                .into(),
                location: Some(format!("Room {} / Shelf {}", index % 20, index % 100)),
                acquisition_date: None,
                notes: None,
                created_at: now.clone(),
                updated_at: now,
            };
            save_document(&tx, "copies", &copy.id, &copy).unwrap();
        }
        for index in 0..5_000 {
            let now = "2026-01-01T00:00:00Z".to_owned();
            let patron = Patron {
                schema_version: SCHEMA_VERSION,
                id: format!("patron-{index:04}"),
                patron_identifier: format!("P{index:05}"),
                name: format!("Patron {index:04}"),
                contact_information: None,
                notes: None,
                active: true,
                created_at: now.clone(),
                updated_at: now,
            };
            save_document(&tx, "patrons", &patron.id, &patron).unwrap();
        }
        for index in 0..50_000 {
            let active = index >= 49_000;
            let copy_index = if active {
                index - 49_000
            } else {
                index % 20_000
            };
            let loan = Loan {
                schema_version: SCHEMA_VERSION,
                id: format!("loan-{index:05}"),
                copy_id: format!("copy-{copy_index:05}"),
                patron_id: format!("patron-{:04}", index % 5_000),
                checkout_date: "2026-09-01".into(),
                due_date: "2026-09-22".into(),
                return_date: (!active).then(|| "2026-09-22".into()),
                closed_at: (!active).then(|| "2026-09-22".into()),
                closure_reason: (!active).then(|| "returned".into()),
                resolution_note: None,
            };
            validate_loan(&loan).unwrap();
            save_document(&tx, "loans", &loan.id, &loan).unwrap();
        }
        tx.commit().unwrap();
        let mut elapsed = Vec::new();
        for _ in 0..100 {
            let started = Instant::now();
            let page = query_books_backend(
                &connection,
                BookQuery {
                    search: "book".into(),
                    page: 1,
                    page_size: 50,
                    sort_by: "title".into(),
                    descending: false,
                    available_only: false,
                },
            )
            .unwrap();
            assert_eq!(page.total, 10_000);
            assert_eq!(page.items.len(), 50);
            elapsed.push(started.elapsed());
        }
        elapsed.sort();
        eprintln!(
            "backend catalog search fixture=10k books/20k copies/5k patrons/50k loans/1k active/1k covers: p95={:?}, max={:?}; excludes UI/webview and process startup",
            elapsed[94], elapsed[99],
        );
    }

    #[test]
    fn validates_unambiguous_calendar_dates() {
        assert!(validate_date("2026-10-15").is_ok());
        assert!(validate_date("15/10/2026").is_err());
        assert!(validate_date("2026-1-5").is_err());
        assert!(validate_date("2026-02-30").is_err());
    }

    #[test]
    fn loan_period_supports_up_to_3650_days_and_applies_to_future_checkouts() {
        assert!(validate_loan_period(1).is_ok());
        assert!(validate_loan_period(3650).is_ok());
        assert!(validate_loan_period(3651).is_err());

        let connection = open_database(Path::new(":memory:")).unwrap();
        let (_, copy, patron) = seed_lending_records(&connection);
        let mut settings = default_settings();
        settings.loan_period_days = 3650;
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "settings", "app", &settings).unwrap();
        tx.commit().unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        let loan = checkout_in_transaction(
            &tx,
            &copy.id,
            &patron.id,
            None,
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(loan.due_date, "2033-12-29");
    }

    #[test]
    fn search_collapses_human_text_whitespace_but_keeps_identifiers_literal() {
        let connection = open_database(Path::new(":memory:")).unwrap();
        let (mut book, mut copy, _) = seed_lending_records(&connection);
        book.title = "Français   Book".into();
        book.authors = vec!["Alice Reader".into()];
        book.tags = vec!["Économie".into()];
        book.identifiers = vec!["001-ABC".into()];
        book.language = Some("Français".into());
        book.pages = Some(240);
        copy.copy_identifier = "É0001".into();
        copy.location = Some("Room   Étage 2".into());
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &book.id, &book).unwrap();
        save_document(&tx, "copies", &copy.id, &copy).unwrap();
        tx.commit().unwrap();

        let books = |search: &str| {
            query_books_backend(
                &connection,
                BookQuery {
                    search: search.into(),
                    page: 1,
                    page_size: 50,
                    sort_by: "title".into(),
                    descending: false,
                    available_only: false,
                },
            )
            .unwrap()
        };
        assert_eq!(books("francais  book").total, 1);
        assert_eq!(books("book alice").total, 0);
        assert_eq!(books("room étage").total, 1);
        assert_eq!(books("economie").total, 1);
        assert_eq!(books("francais").total, 1);
        assert_eq!(books("001-abc").total, 1);
        assert_eq!(books("e0001").total, 0);
        assert_eq!(books("É0001").total, 1);
        assert!(validate_book_fields("Book", None, Some(0)).is_err());
        assert!(validate_book_fields("Book", None, Some(240)).is_ok());
    }

    #[test]
    fn unicode_search_ignores_accents_but_duplicate_identity_does_not() {
        assert_eq!(search_key("Français"), "francais");
        assert_eq!(search_key("ÉCOLE"), "ecole");
        assert_eq!(search_key("Straße"), "strasse");
        assert_eq!(identity_key("francais"), "francais");
        assert_ne!(identity_key("français"), identity_key("francais"));
        assert_eq!(
            normalize_classification(Some("  QA76.73.R87  ".into())).as_deref(),
            Some("QA76.73.R87")
        );
        assert_eq!(normalize_classification(Some("  ".into())), None);
    }

    #[test]
    fn pagination_has_stable_counts_and_bounds() {
        assert!(validate_pagination(0, 50).is_err());
        assert!(validate_pagination(1, 200).is_err());
        let page = page_of(vec![1, 2, 3], 2, 2);
        assert_eq!(page.items, [3]);
        assert_eq!(page.total, 3);
        assert_eq!(page.page, 2);
        assert_eq!(page.page_size, 2);
    }

    #[test]
    fn overdue_requires_an_active_loan_past_its_due_date() {
        let mut loan = Loan {
            schema_version: SCHEMA_VERSION,
            id: "loan-1".into(),
            copy_id: "copy-1".into(),
            patron_id: "patron-1".into(),
            checkout_date: "2026-10-01".into(),
            due_date: "2026-10-14".into(),
            return_date: None,
            closed_at: None,
            closure_reason: None,
            resolution_note: None,
        };
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        assert!(is_overdue(&loan, today).unwrap());
        loan.due_date = "2026-10-15".into();
        assert!(!is_overdue(&loan, today).unwrap());
        loan.return_date = Some("2026-10-14".into());
        loan.closed_at = Some("2026-10-14".into());
        loan.closure_reason = Some("returned".into());
        loan.resolution_note = None;
        loan.due_date = "2026-10-13".into();
        assert!(!is_overdue(&loan, today).unwrap());
    }

    #[test]
    fn checkout_and_return_update_json_documents_together() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let now = now_string();
        let book = Book {
            schema_version: SCHEMA_VERSION,
            id: "book-1".into(),
            title: "Rust for Libraries".into(),
            isbn: None,
            authors: vec!["A. Reader".into()],
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
            updated_at: now.clone(),
            extra: serde_json::Map::new(),
        };
        let copy = CopyRecord {
            schema_version: SCHEMA_VERSION,
            id: "copy-1".into(),
            book_id: book.id.clone(),
            copy_identifier: "LIB-001".into(),
            status: "available".into(),
            location: None,
            acquisition_date: None,
            notes: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let patron = Patron {
            schema_version: SCHEMA_VERSION,
            id: "patron-1".into(),
            patron_identifier: "P-001".into(),
            name: "Sam Reader".into(),
            contact_information: None,
            notes: None,
            active: true,
            created_at: now.clone(),
            updated_at: now,
        };
        let seed = connection.unchecked_transaction().unwrap();
        save_document(&seed, "books", &book.id, &book).unwrap();
        save_document(&seed, "copies", &copy.id, &copy).unwrap();
        save_document(&seed, "patrons", &patron.id, &patron).unwrap();
        seed.commit().unwrap();

        let checkout = connection.unchecked_transaction().unwrap();
        let loan =
            checkout_in_transaction(&checkout, &copy.id, &patron.id, Some("2026-10-22"), today)
                .unwrap();
        checkout.commit().unwrap();
        let checked_out: CopyRecord = read_document(&connection, "copies", &copy.id)
            .unwrap()
            .unwrap();
        assert_eq!(checked_out.status, "checked_out");
        let persisted_loan: Loan = read_document(&connection, "loans", &loan.id)
            .unwrap()
            .unwrap();
        assert_eq!(persisted_loan.return_date, None);

        let returned = connection.unchecked_transaction().unwrap();
        let returned_loan = return_in_transaction(&returned, &loan.id, today).unwrap();
        returned.commit().unwrap();
        let available: CopyRecord = read_document(&connection, "copies", &copy.id)
            .unwrap()
            .unwrap();
        assert_eq!(available.status, "available");
        assert_eq!(returned_loan.return_date.as_deref(), Some("2026-10-15"));
    }

    #[test]
    fn invalid_checkout_rolls_back_without_creating_a_loan() {
        let dir = tempdir().unwrap();
        let connection = open_database(&dir.path().join("library.db")).unwrap();
        let now = now_string();
        let copy = CopyRecord {
            schema_version: SCHEMA_VERSION,
            id: "copy-1".into(),
            book_id: "book-1".into(),
            copy_identifier: "LIB-001".into(),
            status: "available".into(),
            location: None,
            acquisition_date: None,
            notes: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let patron = Patron {
            schema_version: SCHEMA_VERSION,
            id: "patron-1".into(),
            patron_identifier: "P-001".into(),
            name: "Sam Reader".into(),
            contact_information: None,
            notes: None,
            active: true,
            created_at: now.clone(),
            updated_at: now,
        };
        let seed = connection.unchecked_transaction().unwrap();
        save_document(&seed, "copies", &copy.id, &copy).unwrap();
        save_document(&seed, "patrons", &patron.id, &patron).unwrap();
        seed.commit().unwrap();

        let checkout = connection.unchecked_transaction().unwrap();
        let result = checkout_in_transaction(
            &checkout,
            &copy.id,
            &patron.id,
            Some("2020-01-01"),
            NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        );
        assert!(result.is_err());
        drop(checkout);

        let unchanged: CopyRecord = read_document(&connection, "copies", &copy.id)
            .unwrap()
            .unwrap();
        let loans: Vec<Loan> = list_documents(&connection, "loans").unwrap();
        assert_eq!(unchanged.status, "available");
        assert!(loans.is_empty());
    }

    #[test]
    fn refuses_paths_outside_managed_covers() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("covers")).unwrap();
        assert!(managed_cover_path(dir.path(), "../private.png").is_err());
        assert!(managed_cover_path(dir.path(), "covers/cover.png").is_ok());
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let test_env_allowed = cfg!(debug_assertions) || cfg!(feature = "e2e-test");
            let override_path = if test_env_allowed {
                std::env::var_os("PAPER_LIBRARY_APP_DATA")
            } else {
                None
            };
            let testing_mode = override_path.is_some();
            let data_dir = if let Some(value) = override_path {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "PAPER_LIBRARY_APP_DATA must be an absolute path",
                    ).into());
                }
                path
            } else {
                app.path().app_data_dir()?
            };
            fs::create_dir_all(&data_dir)?;
            let settings_path = data_dir.join("application-settings.json");
            let (application_settings, mut startup_error) = match read_application_settings(&settings_path) {
                Ok(settings) => (settings, None),
                Err(error) => (ApplicationSettings::default(), Some(error)),
            };
            let mut app_language = application_settings.language.clone();
            let books_view = application_settings.books_view.clone();
            let default_library_dir = data_dir.join("default-library");
            let mut active_connection = Connection::open_in_memory()?;
            let mut active_library_dir = default_library_dir.clone();
            let mut database_path = PathBuf::new();
            let mut active_lock = None;
            let mut library_open = false;

            let selected_path = application_settings.library_path.as_ref().map(PathBuf::from);
            let legacy_path = data_dir.join("library.sqlite3");
            let candidate = selected_path.or_else(|| {
                (legacy_path.is_file() && data_dir.join("covers").is_dir()).then_some(data_dir.clone())
            });
            if let Some(directory) = candidate {
                if directory.is_dir() {
                    match open_existing_library(&directory) {
                        Ok((connection, lock)) => {
                            if application_settings.library_path.is_none() {
                                match get_settings(&connection) {
                                    Ok(legacy_settings) if ["en", "es", "fr"].contains(&legacy_settings.language.as_str()) => {
                                        app_language = legacy_settings.language;
                                    }
                                    Ok(legacy_settings) => {
                                        startup_error = Some(format!(
                                            "UNSUPPORTED_LANGUAGE: Configured language '{}' is not supported; choose en, es, or fr.",
                                            legacy_settings.language
                                        ));
                                    }
                                    Err(error) => startup_error = Some(error),
                                }
                            }
                            active_connection = connection;
                            active_library_dir = directory.clone();
                            database_path = directory.join("library.sqlite3");
                            active_lock = Some(lock);
                            library_open = true;
                            if application_settings.library_path.is_none() {
                                if let Err(error) = write_application_settings(
                                    &settings_path,
                                    &app_language,
                                    Some(directory.display().to_string()),
                                    &books_view,
                                ) {
                                    startup_error = Some(error);
                                }
                            }
                        }
                        Err(error) => {
                            startup_error = Some(error);
                        }
                    }
                } else {
                    startup_error = Some(format!(
                        "LIBRARY_NOT_FOUND: The last selected library is missing: {}. Locate or create a library to continue.",
                        directory.display()
                    ));
                }
            }
            app.manage(AppState {
                connection: Mutex::new(active_connection),
                database_path: Mutex::new(database_path),
                library_dir: Mutex::new(active_library_dir),
                library_open: Mutex::new(library_open),
                library_lock: Mutex::new(active_lock),
                app_settings_path: settings_path,
                app_language: Mutex::new(app_language),
                books_view: Mutex::new(books_view),
                default_library_dir,
                testing_mode,
                csv_preview: Mutex::new(None),
                metadata_lookups: Mutex::new(HashMap::new()),
                cancelled_operations: Mutex::new(std::collections::HashSet::new()),
                operation_status: Mutex::new(std::collections::HashMap::new()),
                startup_error: Mutex::new(startup_error),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            update_settings,
            update_books_view,
            switch_database,
            create_library,
            open_library,
            library_archive::backup_library,
            library_archive::restore_library,
            operation_status,
            cancel_operation,
            list_books,
            query_books,
            query_authors,
            save_book,
            delete_book,
            list_copies,
            delete_copy,
            add_copy,
            update_copy,
            update_copy_status,
            query_copies,
            list_patrons,
            query_patrons,
            save_patron,
            deactivate_patron,
            checkout,
            return_loan,
            resolve_lost_loan,
            list_loans,
            query_loans,
            set_cover,
            set_cover_from_clipboard,
            set_cover_from_url,
            remove_cover,
            cover_data,
            open_library::lookup_book_metadata,
            open_library::apply_book_metadata,
            csv_import::preview_book_csv,
            csv_import::import_book_csv,
            csv_import::cancel_book_csv
        ])
        .run(tauri::generate_context!())
        .expect("error while running Paper Library");
}
