use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use rusqlite::{backup::Backup, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;
use uuid::Uuid;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::{
    app_error, current_library_dir, ensure_library_open, list_documents, managed_cover_path,
    open_database, validate_loan, validate_open_library, AppState, Book, CopyRecord, Loan,
    OperationStatus, Patron, SCHEMA_VERSION,
};

const ARCHIVE_VERSION: u32 = 1;
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveManifest {
    archive_version: u32,
    schema_version: i64,
    files: Vec<ArchiveFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArchiveFile {
    path: String,
    sha256: String,
    size: u64,
}

fn set_operation(
    state: Option<&AppState>,
    operation_id: Option<&str>,
    phase: &str,
    completed: usize,
    total: usize,
    cancellable: bool,
) -> Result<(), String> {
    if let (Some(state), Some(operation_id)) = (state, operation_id) {
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

fn check_cancelled(state: Option<&AppState>, operation_id: Option<&str>) -> Result<(), String> {
    if operation_id.is_some_and(|id| {
        state.is_some_and(|state| {
            state
                .cancelled_operations
                .lock()
                .map(|cancelled| cancelled.contains(id))
                .unwrap_or(false)
        })
    }) {
        Err("OPERATION_CANCELLED: The operation was cancelled before finalization.".into())
    } else {
        Ok(())
    }
}

fn create_backup_archive(
    state: Option<&AppState>,
    connection: &Connection,
    library_dir: &Path,
    destination: &Path,
    operation_id: Option<&str>,
) -> Result<usize, String> {
    if destination.exists() {
        return Err(
            "BACKUP_EXISTS: Choose a new backup filename; existing backups are never overwritten."
                .into(),
        );
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "INVALID_BACKUP_PATH: Backup file needs a parent directory.".to_owned())?;
    if !parent.is_dir() {
        return Err("BACKUP_DIRECTORY_MISSING: Choose an existing destination directory.".into());
    }
    let cover_paths = referenced_cover_paths(connection)?;
    let temp_archive = parent.join(format!(".backup-{}.partial", Uuid::new_v4()));
    let snapshot_path = parent.join(format!(".snapshot-{}.sqlite3", Uuid::new_v4()));
    let result = (|| {
        set_operation(
            state,
            operation_id,
            "snapshot",
            0,
            cover_paths.len() + 1,
            true,
        )?;
        check_cancelled(state, operation_id)?;
        let snapshot_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&snapshot_path)
            .map_err(app_error)?;
        drop(snapshot_file);
        let mut target = Connection::open(&snapshot_path).map_err(app_error)?;
        Backup::new(connection, &mut target)
            .map_err(app_error)?
            .run_to_completion(64, Duration::from_millis(10), None)
            .map_err(app_error)?;
        validate_restored_documents(library_dir, &target)?;
        target.close().map_err(|(_, error)| app_error(error))?;

        let mut files = Vec::with_capacity(cover_paths.len() + 1);
        let (sha256, size) = sha256_file(&snapshot_path)?;
        files.push(ArchiveFile {
            path: "library.sqlite3".into(),
            sha256,
            size,
        });
        let mut assets = Vec::new();
        for (index, relative) in cover_paths.iter().enumerate() {
            check_cancelled(state, operation_id)?;
            let source = managed_cover_path(library_dir, relative)?;
            if !source.is_file() {
                return Err(format!(
                    "COVER_MISSING: Referenced cover asset '{}' is missing.",
                    relative
                ));
            }
            let (sha256, size) = sha256_file(&source)?;
            files.push(ArchiveFile {
                path: relative.clone(),
                sha256,
                size,
            });
            assets.push((relative.clone(), source));
            set_operation(
                state,
                operation_id,
                "collecting-assets",
                index + 1,
                cover_paths.len() + 1,
                true,
            )?;
        }
        let manifest = ArchiveManifest {
            archive_version: ARCHIVE_VERSION,
            schema_version: SCHEMA_VERSION,
            files,
        };
        let archive_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_archive)
            .map_err(app_error)?;
        let mut writer = ZipWriter::new(archive_file);
        write_zip_file(&mut writer, "library.sqlite3", &snapshot_path)?;
        for (relative, source) in assets {
            check_cancelled(state, operation_id)?;
            write_zip_file(&mut writer, &relative, &source)?;
        }
        writer
            .start_file(
                "manifest.json",
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .map_err(app_error)?;
        writer
            .write_all(&serde_json::to_vec(&manifest).map_err(app_error)?)
            .map_err(app_error)?;
        writer
            .finish()
            .map_err(app_error)?
            .sync_all()
            .map_err(app_error)?;
        verify_archive(&temp_archive, &manifest)?;
        check_cancelled(state, operation_id)?;
        set_operation(
            state,
            operation_id,
            "finalizing",
            cover_paths.len() + 1,
            cover_paths.len() + 1,
            false,
        )?;
        fs::hard_link(&temp_archive, destination).map_err(app_error)?;
        let _ = fs::remove_file(&temp_archive);
        Ok(cover_paths.len() + 1)
    })();
    let _ = fs::remove_file(&snapshot_path);
    if result.is_err() {
        let _ = fs::remove_file(&temp_archive);
    }
    match result {
        Ok(total) => {
            set_operation(state, operation_id, "completed", total, total, false)?;
            Ok(total)
        }
        Err(error) => Err(error),
    }
}

fn sha256_file(path: &Path) -> Result<(String, u64), String> {
    let mut file = File::open(path).map_err(app_error)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(app_error)?;
        if count == 0 {
            break;
        }
        size = size.saturating_add(count as u64);
        hasher.update(&buffer[..count]);
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

fn referenced_cover_paths(connection: &Connection) -> Result<Vec<String>, String> {
    let books: Vec<Book> = list_documents(connection, "books")?;
    let mut paths = Vec::new();
    for book in books {
        if let Some(cover) = book.cover_image {
            if !paths.contains(&cover.path) {
                paths.push(cover.path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn write_zip_file(writer: &mut ZipWriter<File>, name: &str, source: &Path) -> Result<(), String> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    writer.start_file(name, options).map_err(app_error)?;
    let mut file = File::open(source).map_err(app_error)?;
    std::io::copy(&mut file, writer).map_err(app_error)?;
    Ok(())
}

fn verify_archive(path: &Path, manifest: &ArchiveManifest) -> Result<(), String> {
    let file = File::open(path).map_err(app_error)?;
    let mut archive = ZipArchive::new(file).map_err(app_error)?;
    for expected in &manifest.files {
        let mut entry = archive.by_name(&expected.path).map_err(app_error)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = entry.read(&mut buffer).map_err(app_error)?;
            if count == 0 {
                break;
            }
            size = size.saturating_add(count as u64);
            hasher.update(&buffer[..count]);
        }
        if size != expected.size || format!("{:x}", hasher.finalize()) != expected.sha256 {
            return Err(format!(
                "BACKUP_CHECKSUM: Verification failed for '{}'.",
                expected.path
            ));
        }
    }
    Ok(())
}

#[tauri::command(async)]
pub(crate) fn backup_library(
    state: State<'_, AppState>,
    path: String,
    operation_id: Option<String>,
) -> Result<String, String> {
    ensure_library_open(&state)?;
    let destination = PathBuf::from(path);
    if !destination.is_absolute() {
        return Err("INVALID_BACKUP_PATH: Choose an absolute backup file path.".into());
    }
    let library_dir = current_library_dir(&state)?;
    let connection = state.connection.lock().map_err(app_error)?;
    match create_backup_archive(
        Some(&state),
        &connection,
        &library_dir,
        &destination,
        operation_id.as_deref(),
    ) {
        Ok(_) => Ok(destination.display().to_string()),
        Err(error) => {
            if let Some(id) = operation_id.as_deref() {
                let _ = state.operation_status.lock().map(|mut operations| {
                    operations.insert(
                        id.to_owned(),
                        OperationStatus {
                            phase: "failed".into(),
                            completed: 0,
                            total: 0,
                            cancellable: false,
                        },
                    );
                });
            }
            Err(error)
        }
    }
}

fn validate_archive_path(path: &str) -> Result<(), String> {
    if path.contains('\\') || Path::new(path).is_absolute() {
        return Err(format!(
            "BACKUP_PATH_INVALID: Unsafe archive path '{path}'."
        ));
    }
    let parts: Vec<Component<'_>> = Path::new(path).components().collect();
    if parts
        .iter()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!(
            "BACKUP_PATH_INVALID: Unsafe archive path '{path}'."
        ));
    }
    if path != "manifest.json" && path != "library.sqlite3" {
        let components: Vec<_> = Path::new(path).components().collect();
        if components.len() != 2 || components[0].as_os_str() != "covers" {
            return Err(format!(
                "BACKUP_PATH_INVALID: Unsupported archive path '{path}'."
            ));
        }
    }
    Ok(())
}

fn validate_restored_documents(directory: &Path, connection: &Connection) -> Result<(), String> {
    validate_open_library(connection, directory)?;
    let books: Vec<Book> = list_documents(connection, "books")?;
    let copies: Vec<CopyRecord> = list_documents(connection, "copies")?;
    let patrons: Vec<Patron> = list_documents(connection, "patrons")?;
    let loans: Vec<Loan> = list_documents(connection, "loans")?;
    let book_ids: HashSet<String> = books.iter().map(|item| item.id.clone()).collect();
    let copy_ids: HashSet<String> = copies.iter().map(|item| item.id.clone()).collect();
    let patron_ids: HashSet<String> = patrons.iter().map(|item| item.id.clone()).collect();
    let mut active_by_copy: HashMap<String, usize> = HashMap::new();
    for copy in &copies {
        if !book_ids.contains(&copy.book_id) {
            return Err(format!(
                "LIBRARY_REFERENCE_INVALID: Copy '{}' has a missing book.",
                copy.id
            ));
        }
    }
    for loan in &loans {
        validate_loan(loan)?;
        if !copy_ids.contains(&loan.copy_id) || !patron_ids.contains(&loan.patron_id) {
            return Err(format!(
                "LIBRARY_REFERENCE_INVALID: Loan '{}' has a missing copy or patron.",
                loan.id
            ));
        }
        if loan.closed_at.is_none() {
            *active_by_copy.entry(loan.copy_id.clone()).or_default() += 1;
        }
    }
    for copy in &copies {
        let active = active_by_copy.get(&copy.id).copied().unwrap_or(0);
        let valid = match copy.status.as_str() {
            "available" | "withdrawn" => active == 0,
            "checked_out" => active == 1,
            "lost" => active <= 1,
            _ => false,
        };
        if !valid {
            return Err(format!(
                "LIBRARY_STATE_INVALID: Copy '{}' status conflicts with its active loans.",
                copy.id
            ));
        }
    }
    for book in books {
        if let Some(cover) = book.cover_image {
            let path = managed_cover_path(directory, &cover.path)?;
            if !path.is_file() {
                return Err(format!(
                    "COVER_MISSING: Book '{}' references a missing cover asset.",
                    book.id
                ));
            }
            let bytes = fs::read(&path).map_err(|error| {
                format!(
                    "COVER_INVALID: Book '{}' references an unreadable cover asset: {error}",
                    book.id
                )
            })?;
            let (_, media_type) = crate::validate_cover_image(&bytes).map_err(|error| {
                format!(
                    "COVER_INVALID: Book '{}' references an invalid cover asset: {error}",
                    book.id
                )
            })?;
            if media_type != cover.media_type {
                return Err(format!(
                    "COVER_INVALID: Book '{}' cover media type does not match its asset.",
                    book.id
                ));
            }
        }
    }
    Ok(())
}

#[tauri::command(async)]
pub(crate) fn restore_library(
    state: State<'_, AppState>,
    path: String,
    destination: String,
    operation_id: Option<String>,
) -> Result<String, String> {
    let archive_path = PathBuf::from(path);
    let destination = PathBuf::from(destination);
    if !archive_path.is_absolute() || !destination.is_absolute() {
        return Err("INVALID_RESTORE_PATH: Choose absolute archive and destination paths.".into());
    }
    restore_archive(
        Some(&state),
        &archive_path,
        &destination,
        operation_id.as_deref(),
    )
}

fn restore_archive(
    state: Option<&AppState>,
    archive_path: &Path,
    destination: &Path,
    operation_id: Option<&str>,
) -> Result<String, String> {
    if destination.exists() {
        return Err(
            "RESTORE_DESTINATION_EXISTS: Restore requires a new, empty destination directory."
                .into(),
        );
    }
    let metadata = fs::metadata(archive_path).map_err(app_error)?;
    if !metadata.is_file() || metadata.len() > MAX_ARCHIVE_BYTES {
        return Err("BACKUP_TOO_LARGE: Backup archive exceeds the supported size limit.".into());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "INVALID_RESTORE_PATH: Destination needs a parent directory.".to_owned())?;
    if !parent.is_dir() {
        return Err(
            "RESTORE_DIRECTORY_MISSING: Choose an existing destination parent directory.".into(),
        );
    }
    let staging = parent.join(format!(".restore-{}", Uuid::new_v4()));
    fs::create_dir(&staging).map_err(app_error)?;
    let result = (|| {
        set_operation(state, operation_id, "validating-archive", 0, 1, true)?;
        let mut archive =
            ZipArchive::new(File::open(archive_path).map_err(app_error)?).map_err(app_error)?;
        let mut names = HashSet::new();
        for index in 0..archive.len() {
            let entry = archive.by_index(index).map_err(app_error)?;
            let name = entry.name().to_owned();
            validate_archive_path(&name)?;
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(format!(
                    "BACKUP_SYMLINK: Symlink entry '{}' is not allowed.",
                    name
                ));
            }
            if !names.insert(name.clone()) {
                return Err(format!(
                    "BACKUP_DUPLICATE_ENTRY: Archive contains duplicate entry '{}'.",
                    name
                ));
            }
            if entry.size() > MAX_ARCHIVE_BYTES {
                return Err(
                    "BACKUP_TOO_LARGE: An archive entry exceeds the supported size limit.".into(),
                );
            }
        }

        if !names.contains("manifest.json") || !names.contains("library.sqlite3") {
            return Err(
                "BACKUP_INVALID: Archive is missing its manifest or database snapshot.".into(),
            );
        }
        let manifest: ArchiveManifest = {
            let mut file = archive.by_name("manifest.json").map_err(app_error)?;
            if file.size() > 4 * 1024 * 1024 {
                return Err("BACKUP_INVALID: Manifest is too large.".into());
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(app_error)?;
            serde_json::from_slice(&bytes).map_err(|error| format!("BACKUP_INVALID: {error}"))?
        };
        if manifest.archive_version != ARCHIVE_VERSION || manifest.schema_version > SCHEMA_VERSION {
            return Err("BACKUP_VERSION_UNSUPPORTED: Archive format or library schema is newer than this app.".into());
        }
        let expected: HashSet<String> = manifest
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect();
        if expected.len() != manifest.files.len() || !expected.contains("library.sqlite3") {
            return Err(
                "BACKUP_INVALID: Manifest has duplicate paths or lacks its database.".into(),
            );
        }
        for name in &names {
            if name != "manifest.json" && !expected.contains(name) {
                return Err(format!("BACKUP_INVALID: Unlisted archive entry '{name}'."));
            }
        }
        fs::create_dir(staging.join("covers")).map_err(app_error)?;
        for (index, expected_file) in manifest.files.iter().enumerate() {
            check_cancelled(state, operation_id)?;
            validate_archive_path(&expected_file.path)?;
            let destination_file = staging.join(&expected_file.path);
            if let Some(parent) = destination_file.parent() {
                if parent != staging {
                    fs::create_dir_all(parent).map_err(app_error)?;
                }
            }
            let mut entry = archive.by_name(&expected_file.path).map_err(app_error)?;
            if entry.size() != expected_file.size {
                return Err(format!(
                    "BACKUP_SIZE_MISMATCH: Entry '{}' has an unexpected size.",
                    expected_file.path
                ));
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination_file)
                .map_err(app_error)?;
            std::io::copy(&mut entry, &mut output).map_err(app_error)?;
            output.sync_all().map_err(app_error)?;
            let (sha256, size) = sha256_file(&destination_file)?;
            if sha256 != expected_file.sha256 || size != expected_file.size {
                return Err(format!(
                    "BACKUP_CHECKSUM: Verification failed for '{}'.",
                    expected_file.path
                ));
            }
            set_operation(
                state,
                operation_id,
                "extracting",
                index + 1,
                manifest.files.len(),
                true,
            )?;
        }
        check_cancelled(state, operation_id)?;
        let database_path = staging.join("library.sqlite3");
        let connection = open_database(&database_path)?;
        validate_restored_documents(&staging, &connection)?;
        drop(connection);
        set_operation(
            state,
            operation_id,
            "finalizing",
            manifest.files.len(),
            manifest.files.len(),
            false,
        )?;
        fs::rename(&staging, &destination).map_err(app_error)?;
        Ok(destination.display().to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    match result {
        Ok(path) => {
            set_operation(state, operation_id, "completed", 1, 1, false)?;
            Ok(path)
        }
        Err(error) => {
            if let (Some(state), Some(id)) = (state, operation_id) {
                let _ = state.operation_status.lock().map(|mut operations| {
                    operations.insert(
                        id.to_owned(),
                        OperationStatus {
                            phase: "failed".into(),
                            completed: 0,
                            total: 0,
                            cancellable: false,
                        },
                    );
                });
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{now_string, read_document, save_document, CoverImage};
    use tempfile::tempdir;

    fn tiny_png() -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn rejects_traversal_symlink_and_checksum_archives_without_touching_active_library() {
        let dir = tempfile::Builder::new()
            .prefix(".backend-archives-")
            .tempdir_in(".")
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let library = root.join("active");
        crate::tests::create_fixture_library(&library);
        let state = crate::tests::isolated_state(&root);
        crate::open_library_internal(&state, &library).unwrap();
        let settings_before = fs::read(&state.app_settings_path).unwrap();
        let original_books: Vec<Book> =
            list_documents(&state.connection.lock().unwrap(), "books").unwrap();
        for (variant, expected) in [
            ("traversal", "BACKUP_PATH_INVALID:"),
            ("symlink", "BACKUP_SYMLINK:"),
            ("checksum", "BACKUP_CHECKSUM:"),
        ] {
            let archive_path = root.join(format!("{variant}.zip"));
            let mut writer = ZipWriter::new(File::create(&archive_path).unwrap());
            match variant {
                "traversal" => {
                    writer
                        .start_file("../escaped.txt", SimpleFileOptions::default())
                        .unwrap();
                    writer.write_all(b"must never escape").unwrap();
                }
                "symlink" => {
                    writer
                        .add_symlink(
                            "covers/link",
                            "../../escaped.txt",
                            SimpleFileOptions::default(),
                        )
                        .unwrap();
                }
                _ => {
                    let payload = b"checksum fixture";
                    writer
                        .start_file("library.sqlite3", SimpleFileOptions::default())
                        .unwrap();
                    writer.write_all(payload).unwrap();
                    let manifest = ArchiveManifest {
                        archive_version: ARCHIVE_VERSION,
                        schema_version: SCHEMA_VERSION,
                        files: vec![ArchiveFile {
                            path: "library.sqlite3".into(),
                            sha256: "00".repeat(32),
                            size: payload.len() as u64,
                        }],
                    };
                    writer
                        .start_file("manifest.json", SimpleFileOptions::default())
                        .unwrap();
                    writer
                        .write_all(&serde_json::to_vec(&manifest).unwrap())
                        .unwrap();
                }
            }
            writer.finish().unwrap();
            let destination = root.join(format!("restored-{variant}"));
            let error = restore_archive(Some(&state), &archive_path, &destination, Some(variant))
                .unwrap_err();
            assert!(error.starts_with(expected), "{variant}: {error}");
            assert!(!destination.exists());
            assert!(!root.join("escaped.txt").exists());
            assert_eq!(fs::read(&state.app_settings_path).unwrap(), settings_before);
            assert_eq!(current_library_dir(&state).unwrap(), library);
            let books: Vec<Book> =
                list_documents(&state.connection.lock().unwrap(), "books").unwrap();
            assert_eq!(
                serde_json::to_value(books).unwrap(),
                serde_json::to_value(&original_books).unwrap()
            );
            assert!(!fs::read_dir(&root).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".restore-")));
        }
    }

    #[test]
    fn backup_round_trip_captures_wal_and_referenced_cover_assets() {
        let dir = tempdir().unwrap();
        let library_dir = dir.path().join("library");
        fs::create_dir(&library_dir).unwrap();
        fs::create_dir(library_dir.join("covers")).unwrap();
        let cover_relative = "covers/test-cover.png";
        let cover_bytes = tiny_png();
        fs::write(library_dir.join(cover_relative), &cover_bytes).unwrap();
        let database_path = library_dir.join("library.sqlite3");
        let connection = open_database(&database_path).unwrap();
        connection
            .execute_batch("PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        let now = now_string();
        let book = Book {
            schema_version: SCHEMA_VERSION,
            id: "book-archive".into(),
            title: "Archive round trip".into(),
            isbn: None,
            authors: vec!["A. Librarian".into()],
            publisher: None,
            publication_year: None,
            genre: None,
            ddc: Some("005.133".into()),
            lcc: None,
            tags: vec![],
            identifiers: vec![],
            language: None,
            pages: None,
            notes: None,
            cover_image: Some(CoverImage {
                path: cover_relative.into(),
                media_type: "image/png".into(),
            }),
            created_at: now.clone(),
            updated_at: now,
            extra: serde_json::Map::new(),
        };
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &book.id, &book).unwrap();
        tx.commit().unwrap();
        assert!(database_path.with_extension("sqlite3-wal").exists());

        let archive_path = dir.path().join("library-backup.zip");
        create_backup_archive(None, &connection, &library_dir, &archive_path, None).unwrap();
        let restored_path = dir.path().join("restored-library");
        restore_archive(None, &archive_path, &restored_path, None).unwrap();

        let restored = open_database(&restored_path.join("library.sqlite3")).unwrap();
        let restored_book: Book = read_document(&restored, "books", "book-archive")
            .unwrap()
            .unwrap();
        assert_eq!(restored_book.ddc.as_deref(), Some("005.133"));
        assert_eq!(restored_book.cover_image.unwrap().path, cover_relative);
        assert_eq!(
            fs::read(restored_path.join(cover_relative)).unwrap(),
            cover_bytes
        );
    }

    #[test]
    fn rejects_unreadable_cover_assets_during_backup_validation() {
        let dir = tempdir().unwrap();
        let library = dir.path().join("library");
        crate::tests::create_fixture_library(&library);
        let connection = open_database(&library.join("library.sqlite3")).unwrap();
        let mut books: Vec<Book> = list_documents(&connection, "books").unwrap();
        let mut book = books.remove(0);
        let cover_path = "covers/bad.png";
        book.cover_image = Some(CoverImage {
            path: cover_path.into(),
            media_type: "image/png".into(),
        });
        fs::write(library.join(cover_path), b"not an image").unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &book.id, &book).unwrap();
        tx.commit().unwrap();

        let error = validate_restored_documents(&library, &connection).unwrap_err();
        assert!(error.starts_with("COVER_INVALID:"), "{error}");

        book.cover_image = Some(CoverImage {
            path: cover_path.into(),
            media_type: "image/jpeg".into(),
        });
        fs::write(library.join(cover_path), tiny_png()).unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        save_document(&tx, "books", &book.id, &book).unwrap();
        tx.commit().unwrap();
        let error = validate_restored_documents(&library, &connection).unwrap_err();
        assert!(error.starts_with("COVER_INVALID:"), "{error}");
    }
}
