# Paper Library Manager — Product Specification

## 1. Purpose

Build a Rust desktop application for managing a small library's physical book collection. The application keeps a searchable catalog, displays book cover images, tracks individual physical copies, and records who has borrowed each copy and when it is due back.

## 2. Scope and assumptions

- The first release is a local, single-user application. Library data is stored on the computer running the app.
- The application uses Tauri for its desktop shell, with a graphical frontend and Rust backend. Keep business rules and persistence independent of the frontend so they can be tested without launching the desktop app.
- The application manages physical books and copies, not ebooks, serials, or digital lending.
- There is no patron login, network synchronization, or multi-branch support in the first release.
- Store each book, copy, patron, and loan as a JSON document in SQLite. SQLite remains a relational database; JSON documents are the source of truth for domain records, while relational schema may be used for database bookkeeping and derived indexes as specified in DATA-001.
- The interface supports English, Spanish, and French. English is the default.
- Dates are calendar dates stored and entered as ISO 8601 `YYYY-MM-DD` values. The application uses the system's local calendar date when determining whether an active loan is overdue; a system date change takes effect the next time overdue status is evaluated.
- The first release supports macOS, Windows, and Linux as defined in section 6.3. This document describes required behavior, not a claim that every requirement is already implemented.

## 3. Users and goals

### Librarian

- Add and update books and their physical copies.
- Find a book by title, author, ISBN, or copy barcode.
- Register patrons.
- Check out an available copy, see its due date, and record its return.
- Review current and overdue loans.
- Avoid accidentally lending an unavailable, lost, or already-loaned copy.

## 4. Functional requirements

### 4.1 Catalog

**Requirement CAT-001:** Catalog metadata, classifications, copy management, and deletion protection.

1. Create a book record with a required title and optional ISBN, author(s), publisher, publication year, genre, Dewey Decimal Classification (DDC), Library of Congress Classification (LCC), tags, identifiers, language, page count, and notes.
2. Add one or more physical copies to a book record. Each copy has a unique library identifier (barcode or inventory code), an acquisition date (optional), and a status.
3. Edit book metadata and copy details.
4. Search books and copies by exact or partial title, author, ISBN, DDC, LCC, tags, book identifiers, or copy identifier. Search is case-insensitive for text fields.
5. Display a book's metadata, total copy count, and availability count.
6. Allow the librarian to add, replace, view, and remove an optional cover image for each book. Provide a placeholder when a book has no cover.
7. Reject deletion of a copy with any loan history, including resolved losses. Reject deletion of a book while it has copies. Permit deletion of records without these references only after explicit confirmation; use withdrawal rather than deletion for copies with history.
8. Provide separate optional DDC and LCC fields in the book creation/editing form and show them in book details. A book may have either, both, or neither classification. Localize their labels and help text in English, Spanish, and French without translating the stored classification values.
9. Provide an optional free-text `location` on each physical copy (for example, `Room A / Shelf 3 / Bin B`). Allow it to be added, edited, cleared, displayed, and searched independently of DDC and LCC. Blank locations are absent; copies of the same book may have different locations.
10. Provide optional book `tags` and `identifiers` as lists of strings, an optional `language` text value describing the book's content language, and an optional positive whole-number `pages` count. Allow these fields to be added, edited, displayed, searched where applicable, and cleared independently. Tags and identifiers are lists of nonblank values; identifiers are additional identifiers beyond the dedicated ISBN field and are stored as strings, preserving leading zeros and notation. Do not require identifiers to be unique or validate them against an external authority. Treat a blank language as absent; preserve a nonblank value as entered. Do not confuse a book's content language with the application's interface language.
11. Provide **List** and **Grid** display modes for the Books catalog, with a visible control for switching between them. List is the default for a new application profile. Persist the selected mode in application-level settings and restore it after restart; the preference applies across libraries. Both modes must expose the same books, ordering, filters, pagination, availability information, cover state, and book actions. List mode must show a compact row for each book with its cover thumbnail or placeholder, title, first author when present, ISBN when present, total and available copy counts, and access to book details. Switching modes must preserve the current search, filters, sort direction, page, and page size.

### 4.2 Copy status

**Requirement CIRC-001:** Consistent copy and loan state transitions, including loss resolution.

Each copy has one of these statuses:

- `available`: eligible for checkout.
- `checked_out`: currently associated with an active loan.
- `lost`: unavailable and ineligible for checkout; may retain an active loan when lost while borrowed.
- `withdrawn`: permanently removed from circulation and ineligible for checkout.

The application must keep copy status consistent with active loans. A `checked_out` copy must have exactly one active loan. A `lost` copy may have zero or one active loan; `available` and `withdrawn` copies must have none.

Supported transitions:

- An available copy may be checked out, marked lost, or withdrawn.
- Marking a checked-out copy lost keeps its loan active and retains the borrower and due date. The loan remains eligible for overdue reporting.
- Finding a lost copy with an active loan changes its status to `checked_out`; recording its physical return closes the loan and makes it `available`. Finding a lost copy without an active loan makes it `available`.
- Provide a confirmed **Resolve lost loan** operation for a copy that will not be returned. Close the loan with a `lost_resolved` outcome, a resolution date, and a required resolution note; keep the copy `lost`. Do not record a return date or imply physical return. A resolved lost copy may subsequently be withdrawn.
- A copy cannot be withdrawn with an active loan. Withdrawal requires confirmation and is permanent: withdrawn copies cannot re-enter circulation.
- Disallow arbitrary status changes that bypass checkout, return, or loss resolution. Commit related copy and loan changes atomically.

### 4.3 Patrons

**Requirement PAT-001:** Patron registration, editing, search, deactivation, and retained history.

1. Register a patron with a required name and a unique patron identifier. Contact information and notes are optional.
2. Update patron details.
3. Search patrons by identifier or name.
4. Deactivate a patron rather than deleting them when they have loan history. Deactivated patrons cannot receive new loans, but their existing loans can be returned and their history remains accessible.

### 4.4 Loans

1. Check out a copy to an active patron if and only if the copy is available.
2. Record checkout date and due date. The default loan period is 21 days and is configurable.
3. Return an active loan, including one attached to a lost copy. Record the physical return date, close the loan with a `returned` outcome, and mark the copy available.
4. Show active loans for a patron and all active loans in the library.
5. Identify overdue loans when the due date is earlier than the current local date and the loan is still active, including loans for lost copies.
6. Prevent a patron from checking out the same copy twice or a copy from being checked out to multiple patrons.
7. Retain completed loans as history.

The loan period is a positive whole number of calendar days from 1 through 3650. The due date is the checkout date plus the configured number of calendar days; it is not inclusive of the checkout date. Changing the library's default period affects future checkouts only and does not change existing due dates. Accept entered dates only in `YYYY-MM-DD` format; display dates using the selected interface language.

### 4.5 Language and localization

**Requirement I18N-001:** Complete English, Spanish, and French interface localization.

1. Provide English, Spanish, and French translations for all user-facing interface text, including navigation, labels, prompts, success messages, validation errors, and operational errors.
2. Select and persist the language through desktop settings. If no language is configured, use English. Reject unsupported configured language codes with a clear message rather than silently choosing a different language.
3. Keep internal identifiers and data formats language-independent; localize visible labels and messages.
4. Do not translate user-entered catalog or patron data. Format displayed dates according to the selected language while accepting dates in a documented, unambiguous input format.
5. Keep translations in maintainable localization resources rather than scattering translated strings through business logic.

### 4.6 Desktop interface

**Requirement UI-001:** Desktop workflows, explicit errors, and confirmation before destructive operations.

The Tauri desktop interface must provide screens or equivalent workflows for:

- Browsing and searching books, viewing book details and cover images, adding or editing books, and creating a cover from a local file, clipboard image, or HTTP(S) image URL.
- Importing book metadata from a local CSV file, previewing the records, and reviewing validation results before saving.
- Adding and updating physical copies, viewing their status and shelf location, and recording loss, recovery, withdrawal, or loss resolution.
- Registering, searching, editing, and deactivating patrons.
- Checking out and returning copies, and viewing active and overdue loans.
- Choosing the interface language, selecting the Books list/grid display mode, and configuring the loan period and library location.
- Creating, validating, and restoring library backups.

Display clear validation and operational errors. Confirm destructive actions before they are performed. Data-changing operations must report success only after the database transaction commits.

### 4.7 Cover image handling

**Requirement COVER-001:** Managed cover assets and safe image lifecycle.

1. Let the librarian create or replace a cover from any of these sources: a local image selected with a native file picker, JPEG or PNG image bytes currently available on the system clipboard, or an explicitly entered `http://` or `https://` image URL. Clipboard support is image-only; do not interpret clipboard text as a URL. Cover images are optional and associated with the book, not with individual physical copies.
2. Support common image formats (at minimum JPEG and PNG); validate file type and readability before accepting an image. Reject unsupported or corrupt files with a localized, actionable error.
3. Copy accepted images into an application-managed cover-image directory. Keep image bytes as files rather than embedding base64 data in JSON documents or SQLite. Store the managed relative asset path and image metadata in the book JSON document.
4. Display cover images in book search results and book details, with a localized or neutral placeholder when no image is set or an image cannot be displayed.
5. Replacing or removing a cover must not leave orphaned managed image files. Do not delete the current managed image until the book document update succeeds. If saving the new image or updating its document fails, preserve the current cover and clean up any newly created unused image.
6. Restrict displayed image access to application-managed assets and do not expose unrestricted filesystem access to the frontend. Fetch URL sources in the Rust backend, validate them, copy accepted bytes into managed storage, and display only the resulting managed asset. Never store or load the remote URL as the book's displayed cover reference.
7. Name each managed cover file from the book title using the format `<sanitized-title>--<book-id>.<extension>`, preserving the validated image format's `jpg` or `png` extension. The book ID suffix prevents collisions between books with the same title. Sanitize the title for macOS, Windows, and Linux by trimming surrounding whitespace; replacing path separators, control characters, and characters forbidden by any supported platform with `_`; collapsing repeated replacement characters and whitespace; removing trailing spaces and periods; and prefixing Windows-reserved device names with `_`. Limit the title portion as needed to remain within supported filesystem filename limits without removing or truncating the book ID or extension. Use `book` when sanitization leaves no title characters.
8. When a book title changes and the book has a cover, rename the managed cover to match the new sanitized title. Commit the title and new relative cover path together. Do not remove the old cover path until the database update commits; if the rename or database update fails, preserve the previous title, cover reference, and cover file. On successful commit, remove the obsolete managed file using the same safe cleanup rules as cover replacement.
9. For URL sources, allow only `http` and `https`; reject embedded credentials and all other schemes. Resolve the host before connecting and permit only publicly routable addresses. Block loopback, private, link-local, multicast, unspecified, documentation, carrier-grade NAT, and other non-public address ranges for both IPv4 and IPv6. Re-resolve and revalidate every redirect target, reject redirects to a blocked address or unsupported scheme, and limit downloads to five redirects and 30 seconds.
10. Enforce the same encoded-size, dimensions, decoded-allocation, format, and corruption checks for file, clipboard, and URL sources. For URL downloads, reject an oversized `Content-Length` before reading and enforce the encoded-size limit while streaming even when the length is absent or incorrect. Treat response headers as hints only; validate the downloaded bytes before installing the cover. Do not retain partial downloads after cancellation or failure.

### 4.8 CSV book import

**Requirement CSV-001:** Books-only delimited-file preview, validation, confirmation, and atomic import.

1. Allow the librarian to select a local CSV file using a native file picker. Import book metadata only; do not import physical copies, patrons, loans, or cover images. Imported books initially have no copies or cover image.
2. Accept UTF-8 files whose fields are separated by either a comma (`,`) or a pipe (`|`), with or without a UTF-8 byte order mark, and a header row. Use the same delimiter for the header and all records in a file, and show the delimiter used in the import preview. Support quoted fields, escaped double quotes, embedded delimiters and line breaks, and both LF and CRLF line endings. A delimiter inside a quoted field is part of that field's value. The semicolon separator for author names remains unchanged with either field delimiter.
3. Use stable, language-independent column names: required `title`; optional `isbn`, `authors`, `publisher`, `publication_year`, `genre`, `ddc`, `lcc`, `tags`, `identifiers`, `language`, `pages`, and `notes`. An `authors` cell contains names separated by semicolons. `tags` and `identifiers` cells contain semicolon-separated values. Blank optional cells represent absent values; blank authors, tags, or identifiers represent empty arrays. Preserve ISBNs, classifications, and identifiers as strings, including leading zeros, decimal precision, letters, and internal spacing. Parse nonblank `pages` as a positive whole number. Show DDC, LCC, tags, identifiers, language, and pages in the import preview.
4. Require the `title` header and reject duplicate headers. Report unknown columns in the preview rather than silently discarding them. Ignore completely empty records.
5. Preview the parsed books before importing. Show the number of candidate books and identify errors by CSV record number and column name. Validate required titles, publication years as non-negative integers, and all other applicable book rules using the same Rust business logic as manual entry.
6. Flag possible duplicates within the file and against the current catalog when a nonblank normalized ISBN matches, or when title and authors match case-insensitively. Do not silently merge or overwrite existing books. Require explicit confirmation to import flagged records as separate books.
7. Do not write any records during preview. Require confirmation before import and block import while parsing or validation errors remain. Commit all confirmed books in one SQLite transaction; any failure must roll back the entire import. Cancellation must leave the library unchanged.
8. Generate internal IDs, schema versions, and timestamps in the application, and persist imported books as the same JSON documents used by manually created books.
9. After a successful commit, show the imported count and refresh the catalog. Localize the import workflow, validation messages, duplicate warnings, and completion summary in English, Spanish, and French without translating CSV headers or user-entered data.
10. Detect comma or pipe delimiters by parsing the header with quote-aware rules. Choose automatically only when exactly one delimiter produces a valid, unique header set containing `title`. If both or neither are valid, require manual selection; always offer a comma/pipe override and reparse the file after changes. A title-only file may require manual selection.
11. Limit input to 20 MiB and 10,000 nonempty book records; reject larger inputs with a localized explanation before any writes. Display parsing/import progress and keep the UI responsive. Cancellation during any cancellable pre-commit work must stop work and discard pending changes. Clearly identify the non-cancellable commit phase and report its actual committed or rolled-back result.
12. Bind each preview to its selected library and input snapshot. Changing the library or input invalidates the preview. Recheck duplicates and validation in the same write transaction used to import.

Unknown columns must be explicitly acknowledged in the preview before import can proceed; acknowledgement does not import those columns. Cancellation is available during file reading, parsing, validation, and any pre-commit staging. Once the atomic database transaction begins, cancellation is disabled until it commits or rolls back; the UI must clearly identify this non-cancellable phase and report the actual result.

### 4.9 Search, sorting, and lists

**Requirement SEARCH-001:** Predictable Unicode search, stable ordering, and bounded results.

- For catalog, patron, and copy-location searches, use Unicode normalization and case folding, not SQLite's ASCII-only case matching. Ignore accents for human-readable names, titles, and locations: `francais` must find `français`. Preserve the original stored text.
- Treat a nonempty search query as a literal substring after canonical Unicode normalization, locale-independent Unicode case folding, removal of combining diacritical marks from human-readable names/titles/locations, and collapsing runs of whitespace to one space. Apply the same key generation to stored values and queries. Search each supported field independently; a query spanning fields is not a match. Do not add fuzzy or token-reordering matches. DDC and LCC retain punctuation and do not ignore accents.
- Treat ISBNs, book identifiers, and copy/patron identifiers as strings; do not strip leading zeros. Normalize ISBN spaces and hyphens for ISBN lookup. DDC and LCC searches preserve punctuation; comparisons are case-insensitive.
- Search tags and language as human-readable text using the catalog's Unicode normalization and case-folding rules. Search book identifiers as strings, preserving leading zeros and punctuation; compare case-insensitively without ISBN-specific normalization.
- Use canonically Unicode-normalized, case-folded title/author comparison for CSV duplicate detection, with author order ignored and accents preserved; collapse surrounding and repeated whitespace for this comparison. Normalize ISBNs by removing spaces/hyphens and ignoring letter case. Do not use accent-insensitive matching or fuzzy search as a duplicate identity rule.
- Default to book title, patron name, and due date ascending for books, patrons, and active loans respectively. Each active-loan row is sorted by that loan's due date. Offer ascending/descending sorting on title, author, publication year, and copy location where applicable; use stable IDs to break ties. Missing values sort last in either direction; author sorting uses the first listed author, and text sorting uses the same normalized text keys as search.
- Provide filters for books with available copies, copy status/location, active or completed loans, overdue loans, and loans for a selected patron. Lost active loans must appear in applicable patron and overdue lists.
- Paginate lists at 50 records per page by default, with 25/50/100 choices and visible total counts. Reset to the first page when a query or filter changes; pagination must not duplicate or omit unchanged results.

### 4.10 Portable libraries and settings

**Requirement STORE-001:** Each library is a portable directory containing `library.sqlite3` and its own `covers/` directory. Cover paths are relative to that library, never another library or a machine-specific application data directory.

- Provide separate **Create library** and **Open library** actions. Creation uses a new or empty directory and never replaces existing contents. Opening validates an existing library before switching; a failed open leaves the current library unchanged.
- Persist the selected library directory in application-level settings and reopen it after restart. Store interface language and the Books list/grid display preference at application level; store the default loan period with the library. Default a missing display preference to List for new and older application settings. Changing libraries invalidates pending edits and CSV previews, with confirmation before discarding unsaved work.
- A library lock represents exclusive write access. If another application instance holds the lock, do not open that library for writing. Detect and report a lock as stale only when the platform's lock mechanism can establish that its owning process is no longer running; otherwise require the user to close the other instance or choose another library. Do not automatically remove an ambiguous lock.
- On first launch offer creation at a documented platform-default location or selection of an existing library. If the last library is missing or inaccessible, offer retry, locate, or create actions; do not silently create an empty replacement.
- Moving the complete library directory must preserve books, loans, and cover references on all supported operating systems. Detect a library already open for writing by another app instance and report the lock rather than permitting conflicting asset changes.

### 4.11 Backup and restore

**Requirement BACKUP-001:** Provide built-in local backup and restore of the complete library.

- Produce a versioned archive containing a consistent SQLite snapshot, all referenced cover assets, and a manifest with library/schema versions and file checksums. Include library loan settings; do not overwrite application language or machine-specific preferences during restore.
- Pause library mutations while capturing the database and covers. Use SQLite's supported backup mechanism or an equivalent consistent snapshot; copying only a live database file without accounting for WAL is not sufficient.
- Verify the snapshot, referenced assets, and checksums before reporting success. Write to a temporary archive and finalize only when complete. Surface disk-full, permission, and missing-asset errors; never present an incomplete backup as successful or overwrite an existing backup without confirmation.
- A backup cannot succeed if a book references a missing or unreadable cover asset. Restore into a new directory, not over the active library. Validate the manifest, schema compatibility, checksums, SQLite integrity, JSON records, and cover references before offering to open the restored library; reject archives with missing or invalid referenced assets.
- Reject archive paths that escape the destination, absolute paths, and symlink entries. Clean up only staged restore artifacts on failure; keep the original library and backup unchanged.
- Provide progress and cancellation during snapshot creation, asset collection, validation, and archive staging. Disable cancellation only during finalization, clearly indicate that phase, and report whether the archive was finalized. Provide a localized completion/error summary. Test backup while SQLite WAL contains uncheckpointed writes and round-trip restoration across supported platforms.

## 5. Data model and JSON document storage

**Requirement DATA-001:** Versioned JSON domain records, transactional integrity, and backward-compatible migrations.

Store domain records as JSON documents in a SQLite table, for example `documents(collection, id, body)`. `collection` identifies the document type (`books`, `copies`, `patrons`, or `loans`); `id` is a stable unique identifier; and `body` contains the complete JSON document. Require valid JSON using SQLite's JSON support (for example, `json_valid(body)`). A small relational migration/version table is allowed for database bookkeeping, but catalog and circulation records must be stored as JSON documents, not decomposed into relational domain tables.

Documents must include a document schema version and timestamps where applicable. Calendar dates use ISO 8601 `YYYY-MM-DD`; timestamps use RFC 3339 with a UTC offset. The minimum fields are:

- **Book**: `id`, `title`, optional `isbn`, `authors` (array of names), optional `publisher`, `publication_year`, `genre`, `ddc` (Dewey Decimal Classification string), `lcc` (Library of Congress Classification string), `tags` (array of strings), `identifiers` (array of strings), optional `language` (content language text), optional `pages` (positive integer page count), and `notes`, optional `cover_image` metadata (managed relative path and media type), `created_at`, `updated_at`.
- **Copy**: `id`, `book_id` referencing a book document, unique `copy_identifier`, `status`, optional `location`, `acquisition_date`, and `notes`, `created_at`, `updated_at`.
- **Patron**: `id`, unique `patron_identifier`, `name`, optional contact information and `notes`, `active`, `created_at`, `updated_at`.
- **Loan**: `id`, `copy_id` and `patron_id` referencing documents, `checkout_date`, `due_date`, nullable `return_date`, nullable `closed_at` (calendar date), nullable `closure_reason` (`returned` or `lost_resolved`), and optional `resolution_note` (required for `lost_resolved`).

Use SQLite JSON extraction and indexes on frequently searched or constrained fields, such as book title/ISBN, copy identifier/status, patron identifier/name, and loan copy/patron/due/return dates. Enforce unique copy and patron identifiers, and at most one active loan per copy, with suitable SQLite expression or partial indexes where supported. Use case-insensitive search indexes or comparisons for searchable text. Author arrays must be searchable by exact or partial author name; the implementation may use SQLite JSON functions or an appropriate derived index without making relational rows the source of truth.

Because references are IDs inside JSON rather than relational foreign keys, the application must validate referenced documents and maintain referential integrity in transactions. Required integrity rules:

- Validate document shape, required fields, field types, supported status values, and document schema version when reading and writing records.
- A loan's due date cannot precede its checkout date.
- A loan is active when `closed_at` is absent/null, not merely when `return_date` is null. At most one active loan may exist for each copy, including lost copies; database indexes and queries must use this definition.
- A copy with `checked_out` status has exactly one active loan. A lost copy has zero or one; available and withdrawn copies have none.
- Returned loans have equal `return_date` and `closed_at` dates and `closure_reason = returned`. Lost-resolved loans have no return date and require a resolution date and note. Closing dates cannot precede checkout; active loans have no closure reason or return date.
- Migrate older returned loans by deriving `closed_at` from `return_date` and setting the outcome to `returned`. Older loans with no return date remain active. Preserve existing IDs, dates, and history.
- Checkout and return update the loan and copy documents atomically. Failed validation or writes must leave both documents unchanged.
- Deletion must not leave dangling references or erase loan history; enforce the rejection rules in CAT-001.

ISBN storage should preserve its original printable form; ISBN validation may normalize hyphens and spaces for comparison. Do not require ISBN because older books may not have one.

Store DDC and LCC as nullable strings, not numeric values. Trim surrounding whitespace and treat blank values as absent; otherwise preserve the librarian-entered notation (for example, DDC `005.133` and LCC `QA76.73.R87`). Do not require classification values to be unique, infer them automatically, or validate their assignment against an external classification authority. Existing book documents without these fields must remain readable and editable, treating missing fields as absent and preserving all other data. Include classification fields in catalog search and document their use in forms and CSV imports.

Store book `tags` and `identifiers` as arrays of strings. Trim surrounding whitespace from each value and omit blank entries. Keep identifiers as strings, preserve leading zeros and notation, and do not require uniqueness or validate them against an external authority. Store `language` as nullable text describing the book's content language; trim surrounding whitespace and treat blank values as absent, otherwise preserve the entered value without requiring a particular language-code standard. Store `pages` as a nullable positive integer. Older book documents without these fields remain readable and editable, with missing fields treated as absent and all other data preserved. Include these fields in the book form, catalog search as specified in SEARCH-001, CSV preview/import, and persistence validation.

Store cover image files in the library's managed `covers/` directory alongside, but not inside, the SQLite database; the book's JSON document stores only the relative path and metadata. Include cover assets in built-in backup and restore. Removing a book or cover must clean up its managed asset safely without deleting files outside the selected library's managed directory.

Managed cover paths use `covers/<sanitized-title>--<book-id>.<extension>` as defined in COVER-001. Title edits must update the book title and managed relative cover path atomically while preserving the prior committed file and reference on failure. Existing cover files created under an older naming scheme remain readable; rename them to the title-based format the next time the associated title or cover is changed.

## 6. Non-functional requirements

- Implement the application in stable Rust.
- Use a maintained Rust SQLite library with JSON support and apply versioned database migrations at startup. Migrations must preserve existing documents.
- Create and open portable library directories as specified in STORE-001; document the platform-default location.
- Use transactions for multi-document operations and parameterized SQL for all user-supplied values.
- Never silently replace or delete an existing database. Database initialization and migration failures must be reported clearly.
- Keep database access and business rules independent of Tauri command handlers and frontend presentation.
- Expose only the operations needed by the frontend through Tauri commands, validate all command inputs in the Rust backend, and scope filesystem access to the selected library, application settings, and user-selected image, CSV, and backup/restore paths. Restrict cover-image network access to the validated public HTTP(S) retrieval defined in COVER-001. Do not expose unrestricted file or network access or load remote code in the webview.
- Handle empty databases, missing records, duplicate identifiers, malformed dates, and database I/O errors without panics.
- Ship and test complete English, Spanish, and French desktop-interface translations.
- Provide automated tests for domain rules and database persistence. Tests must use temporary databases and must not modify a user's real library data.
- Document installation/build, database and cover-image locations and backup, desktop workflows, language selection, and supported loan-period configuration. Include comma-separated and pipe-separated book CSV samples and document encoding, supported field delimiters, quoting rules, headers, author separator, validation, duplicate handling, and transactional import behavior.

### 6.1 Required application tests

**Requirement TEST-001:** Automated verification, regression protection, and honest release reporting.

Tests are part of the application deliverable, not an optional follow-up. Every new feature and bug fix must include appropriate automated tests; bug fixes must include a regression test where reproducible.

- **Rust unit tests:** Cover book and classification validation, copy status transitions, patron eligibility, checkout/return rules, overdue date boundaries, and CSV parsing and duplicate detection.
- **SQLite integration tests:** Use real temporary SQLite databases to verify JSON document persistence after reopening, uniqueness and reference integrity, migrations and older-document compatibility, and full transaction rollback on failed operations.
- **Cover image tests:** Cover valid JPEG/PNG files, clipboard image bytes, HTTP and HTTPS retrieval, redirects, blocked private/non-public addresses, DNS rebinding defenses, credentials and unsupported schemes, timeouts, streaming size limits, unsupported or corrupt images, dimension/allocation limits, path restrictions, title-based filenames and renames, replacement/removal, cancellation, and cleanup failures without loss of the current cover. Use controlled local HTTP fixtures and mocked DNS/network boundaries; tests must not contact external services.
- **Frontend tests:** Verify forms, navigation, loading and error states, destructive-action confirmation, catalog and patron search, classification fields, Books list/grid switching and application-level persistence, state preservation while switching modes, and import preview/confirmation/cancellation. Cover all three interface languages, translation-key completeness, and localized dates and errors.
- **CSV workflow tests:** Exercise both comma and pipe delimiters, BOM and Unicode, quoting and multiline fields, headers, invalid values, unknown-column acknowledgement, duplicate confirmation, cancellation, and atomic import failures.
- **End-to-end tests:** Exercise the desktop UI through the real Tauri command layer and Rust backend, using isolated test data: create/search/edit a book, manage classifications and a cover, add a copy and patron, check out and return a book, and preview/import CSV records. Mocked browser tests may supplement these tests but do not replace real desktop integration testing.
- **Startup smoke test:** Launch the application and verify that its window and catalog load without a panic, plugin initialization failure, missing capability, or frontend startup error. Compilation alone is not sufficient.
- **Recovery and portability tests:** Verify library selection after restart, movement to another directory, lock handling, missing assets, interrupted image replacement, failed migrations, disk-full failures, backup consistency, and failed restore isolation.
- **Accessibility and performance tests:** Verify the behaviors and measurable thresholds in sections 6.2 and 6.4. Exercise supported platform builds and real desktop workflows on the platform matrix in section 6.3.
- **Repeatability and reporting:** Use temporary databases and asset directories, fixed dates where relevant, and local fixtures; never modify a user's real library or depend on external services. Document commands to run each test suite and the production build. Required tests and build checks must pass before declaring the release complete; report any platform-specific tests that could not run rather than claiming they passed.

### 6.2 Accessibility

**Requirement A11Y-001:** The complete desktop workflow must be usable without a mouse and must meet WCAG 2.2 AA criteria applicable to the webview interface.

- Provide accessible names, associated labels, visible keyboard focus, logical tab order, and keyboard access to all forms, menus, tables, and actions.
- Dialogs move focus inside, keep focus within while open, support Escape when cancellation is allowed, and restore focus to their opener when closed.
- Announce validation errors, operation outcomes, and progress to assistive technology. Connect field errors to their inputs and provide a summary for multi-field failures.
- Meet AA text/non-text contrast requirements. Indicate availability, overdue status, and errors with text or icons as well as color.
- At 200% text size, preserve access to all controls and data without clipped content; allow scrolling where needed. Do not require hover to reveal essential actions.
- Test representative workflows with VoiceOver on macOS, NVDA on Windows, and Orca on Linux, alongside automated accessibility checks. Record versions and manual results.

### 6.3 Supported platforms and packaging

**Requirement PLATFORM-001:** Ship and verify a native Tauri desktop release for all of:

| Platform | Minimum release baseline | Architecture | Deliverable |
| --- | --- | --- | --- |
| macOS | macOS 13 | Apple Silicon arm64 and Intel x86_64 | Signed, notarized app in DMG |
| Windows | Windows 11 | x86_64 | Signed installer with WebView2 prerequisite handling |
| Linux | Ubuntu 24.04 LTS | x86_64 | Debian package with documented WebKitGTK/system dependencies |

Linux support in the first release is limited to the named distribution baseline; other distributions are best-effort. Verify installation, startup, core workflows, and backup/restore on each baseline and a current supported OS version where distinct. Test both macOS architectures. Record exact OS, webview, Rust, Tauri, and frontend tool versions in release evidence. Development builds may be unsigned but do not satisfy distribution-signing acceptance.

### 6.4 Performance and resource limits

**Requirement PERF-001:** Measure a production build using a deterministic local fixture with 10,000 books, 20,000 copies, 5,000 patrons, and 50,000 loans, including 1,000 active loans and 1,000 representative cover images.

- Reference hardware: 4 or more CPU cores, 8 GiB RAM, and a local SSD; record actual hardware, OS, fixture size, and cache conditions for every benchmark.
- From process launch to an interactive first catalog page (catalog results rendered and keyboard interaction available): at most 3 seconds at the 95th percentile over 20 warm launches. Calculate percentiles using the nearest-rank method (`rank = ceiling(p × sample_count)`). Record a separate first launch after reboot; exclude installation and migrations from this threshold and report them separately.
- From submitting a search/filter/page change to all 50 results being rendered and available for interaction: at most 500 ms at the 95th percentile over 100 operations on the fixture, using the same nearest-rank method.
- Parse and preview a valid 10,000-record CSV within 10 seconds and commit it within 15 seconds on the reference hardware, including duplicate checks against the fixture. Record five runs for each delimiter; every measured run must meet the bound.
- Keep scrolling and cancellation usable during clipboard processing, URL download, image validation, import, backup, and restore; run blocking work off the UI event loop. Show progress for operations taking more than 1 second.
- Cover creation from files, clipboard images, or URLs accepts JPEG/PNG content up to 10 MiB encoded, dimensions up to 8,000 pixels per side, and decoded image allocations up to 128 MiB. Validate before installing the new asset; explain rejected limits in the selected language.
- Bound preview rendering and load list images lazily. Do not load or transfer the entire library and all cover bytes just to display one page.

### 6.5 Failures, recovery, and privacy

**Requirement RECOVERY-001:** Preserve committed data and provide explicit recovery paths instead of silent defaults or success-shaped failures.

- Missing or unreadable covers show a placeholder and a localized repair message with replace/remove actions. Do not discard book metadata or silently erase its asset reference.
- Corrupt JSON, unsupported document/database versions, or failed integrity checks must identify the affected record or library and prevent unsafe writes. Offer backup/restore or library selection; never silently skip corrupt records, downgrade a newer database, or reset it.
- Back up before a migration. Apply each migration transactionally and record versions only after success. A migration failure must preserve the prior library and report how to recover.
- Report database contention after a bounded wait of at most 5 seconds and offer retry. A retry must not duplicate a completed checkout, return, resolution, or import.
- For permission errors and full disks, preserve the last committed state and show the failed operation, an actionable localized explanation, and optional technical details. Distinguish a failed database commit from a committed update whose asset cleanup or UI refresh failed.
- Stage new cover assets before updating references. Keep the old asset until commit; after commit, queue old assets for cleanup. On restart reconcile interrupted operations, removing only unreferenced files proven to be application-owned. Never delete the cover referenced by committed data because later cleanup failed.
- Archive extraction and cover reads/writes must reject traversal and symlink escapes from managed storage. Deletion cleanup must target only known owned assets.
- Keep diagnostic logs local, redact patron/contact data and book notes, and never log complete imported files, clipboard image bytes, downloaded image bytes, or URL credentials. Do not upload telemetry or library data. Explain that retrieving a cover URL sends a network request to that server and may disclose the user's IP address and request metadata. Libraries and backup archives are not encrypted by the app and rely on operating-system access controls.

## 7. Out of scope for the first release

- Network access other than explicit public HTTP(S) cover-image retrieval, mobile app, multi-user permissions, and synchronization.
- Reservations, renewals, fines, notifications, and acquisition/inventory accounting.
- CSV export, non-CSV catalog import formats, and CSV import of copies, patrons, loans, or cover images. Whole-library backup archive restoration is in scope and is not a catalog import.
- Multiple library branches and multiple copies of the same barcode.

## 8. Acceptance criteria

The first release is acceptable when:

1. A librarian can create and search for a book, add copies, register a patron, check out an available copy, and return it using the Tauri desktop interface.
2. Books, copies, patrons, and loans are persisted as valid JSON documents in SQLite and remain available after restarting the application.
3. Searching by partial title or author finds matching books without case sensitivity.
4. An unavailable copy, inactive patron, duplicate identifier, or invalid due date is rejected with a clear error and no partial data changes.
5. A checked-out copy cannot be checked out again, and an active loan cannot be duplicated for that copy.
6. Returning a loan records its return date and makes the copy available.
7. Active and overdue loan listings correctly reflect persisted loan data.
8. A librarian can create a cover from a selected local file, clipboard JPEG/PNG image, or public HTTP(S) image URL, then display, replace, and remove it. Private or unsafe URL targets and invalid images are rejected. Covers use the required title-based managed filename, and title changes rename them without leaving orphaned files or losing the current cover on failure.
9. The interface provides English, Spanish, and French labels, messages, validation errors, and operational errors; selecting each language produces the corresponding localized output.
10. Automated tests cover JSON document persistence and validation, the checkout/return lifecycle, overdue calculation, uniqueness constraints, cover image management, localization, and transaction rollback on failed operations.
11. A new database is initialized safely, and an existing database is migrated without losing its documents.
12. A librarian can preview and confirm valid books-only CSV imports using either comma (`,`) or pipe (`|`) field delimiters, including accented characters, quoted delimiters, multiline notes, semicolon-separated authors, and ISBNs with leading zeros. The preview shows the delimiter used. Imported books are searchable and persist as JSON documents after restart. Automated tests cover both field delimiters and preservation of quoted delimiter characters.
13. Missing or duplicate headers, malformed CSV, invalid encoding, and invalid field values are reported before any database changes. Unknown columns require explicit acknowledgement and are not imported. Possible duplicates require explicit confirmation and never overwrite existing books.
14. Cancelling an import before the non-cancellable transaction begins, or encountering a write failure, leaves the database unchanged. Once the transaction starts, the UI reports its actual committed or rolled-back result. Automated tests cover parsing, validation, duplicate detection, successful persistence, and full transaction rollback.
15. A librarian can add, edit, clear, display, search, and CSV-import DDC and LCC independently. Values such as `005.133` and `QA76.73.R87` retain their notation after saving and restarting. A librarian can also manage and CSV-import book tags, identifiers, content language, and page count; identifiers retain leading zeros and notation. Existing books without these fields continue to work. Automated tests cover persistence, search, CSV preview/import, blank values, page-count validation, and compatibility with older documents.
16. The required unit, database integration, frontend, end-to-end, and startup tests in section 6.1 are included and pass, alongside the production build. Test commands and results are documented, with any unexecuted platform-specific checks explicitly identified.
17. Marking a borrowed copy lost preserves its active loan and overdue eligibility. Returning it or resolving its loss closes the loan with the correct distinct outcome. Withdrawal is blocked while a loan is active and cannot be reversed.
18. A complete library can be moved, reopened after restart, backed up with live WAL data, and restored into a new directory without losing documents, library settings (including the loan period), or covers. Restore does not overwrite application-level language or machine-specific preferences. Failed opening, migration, backup, or restore leaves the original library usable.
19. Shelf locations, Unicode/accent-insensitive search, sorting, filters, and pagination behave as specified. Comma/pipe selection, ambiguity handling, limits, progress, and cancellation are verified.
20. Accessibility checks, the supported-platform test matrix, and the performance measurements in sections 6.2–6.4 meet their requirements, with recorded evidence rather than build-only or mocked-browser claims.
21. A librarian can switch the Books catalog between List and Grid modes without changing the current query, filters, sort, page, or page size. List is the default for a new or older application profile, the selected mode persists across restarts and library changes, and both modes expose equivalent catalog records, availability, cover state, and book actions.

## 9. Requirement traceability and release evidence

**Requirement TRACE-001:** Keep the following stable IDs when requirements change. Existing numbered items inherit the ID of their section; section 4.4 is part of CIRC-001 and general section 6 integrity requirements are part of DATA-001/RECOVERY-001. Add new IDs rather than recycling retired ones.

This is a verification map, not an implementation-status report:

| Requirement | Scope | Acceptance criteria | Required verification |
| --- | --- | --- | --- |
| CAT-001 | Books, classifications, copies, shelf locations, deletion, list/grid display | 1, 3, 4, 15, 19, 21 | Domain, persistence, frontend, real desktop workflows |
| CIRC-001 | Checkout, return, active/completed/lost loans, withdrawal | 1, 4–7, 17 | State-transition, date-boundary, rollback, real desktop tests |
| PAT-001 | Patron lifecycle and history | 1, 4, 7 | Domain, persistence, frontend, real desktop tests |
| I18N-001 | English, Spanish, French | 9 | Translation completeness, localized dates/errors, UI tests |
| UI-001 | Navigation, forms, confirmations, outcomes, catalog display modes | 1, 4, 8, 9, 21 | Frontend and real desktop workflows |
| COVER-001 | Cover selection, validation, storage, cleanup | 8, 18 | Image fixtures, failure injection, crash recovery |
| CSV-001 | Both delimiters, preview, duplicate control, atomic writes | 12–15, 19 | Parser, UI, limits/cancel, rollback and real desktop tests |
| SEARCH-001 | Unicode search, ordering, filters, pagination | 3, 15, 19 | Unicode fixtures, stable page-boundary tests, UI tests |
| STORE-001 | Portable libraries and persisted application preferences | 2, 18, 21 | Move/reopen, switch/cancel, preference persistence, missing/locked library tests |
| BACKUP-001 | Consistent backup and validated restoration | 18 | Live-WAL snapshot, round trip, archive validation, failure tests |
| DATA-001 | JSON integrity, references, versioning, transactions | 2, 4–6, 10, 11, 18 | Real SQLite, old fixtures, migration and rollback tests |
| TEST-001 | Required test suites and regression coverage | 10, 14, 16 | Suite results, reproducible commands, platform evidence |
| A11Y-001 | Accessible desktop interactions | 20 | Automated checks plus keyboard and screen-reader runs |
| PLATFORM-001 | Supported OS/architecture and installable releases | 16, 20 | Install, real startup/workflow, package/signature checks |
| PERF-001 | Measured latency and resource bounds | 19, 20 | Reproducible fixture and recorded benchmark results |
| RECOVERY-001 | Failure isolation, recovery, privacy | 8, 11, 14, 18 | Injected I/O/migration/cleanup failures and recovery tests |
| TRACE-001 | Honest status and requirement-to-test mapping | 16, 20 | Release verification report |

For each release, maintain a separate verification report keyed by requirement ID with implementation status (`not implemented`, `partial`, or `implemented`), verification status (`not run`, `failed`, or `passed`), linked test names/results, tested commit or source snapshot, platform, and execution date. Do not mark a requirement implemented merely because a test or executable builds. Partial, failed, and unexecuted requirements remain visible; the first release is complete only when all in-scope requirements have passing evidence on their required platforms.

The first-release scope in this specification is cumulative: all requirements stated as first-release requirements are release-blocking. A staged rollout or deferral requires an explicit scope revision that identifies deferred requirement IDs and updates the affected acceptance criteria and traceability entries. Acceptance-criterion numbers are stable references; append new criteria rather than renumbering existing criteria, and explicitly mark retired criteria rather than reusing their numbers.
