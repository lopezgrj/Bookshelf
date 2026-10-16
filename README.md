# Paper Library

A local Rust/Tauri 2 desktop application for physical books, cover images, copies, patrons and loans. Books and circulation records are versioned JSON documents in SQLite; the JavaScript webview is available in English, Spanish and French.

## Development

Install stable Rust, Node.js/npm and the [Tauri platform prerequisites](https://tauri.app/start/prerequisites/), then:

```sh
npm ci
npm run tauri dev
```

Build a production binary with `npm run tauri -- build --no-bundle`, or platform bundles with `npm run tauri build`. Builds are native; a macOS binary is not a Windows or Linux installer. See [VERIFICATION.md](VERIFICATION.md) for actual verification and release blockers. An unsigned development build does not meet the signed-distribution requirements in [SPECIFICATION.md](SPECIFICATION.md).

The verified development macOS arm64 DMG is generated at `src-tauri/target/release/bundle/dmg/Paper Library_0.1.0_aarch64.dmg`. It is not Developer ID signed or notarized and is not a release-approved distribution.

## Portable libraries

On first launch choose **Create library** in a new/empty directory, or **Open library** in a directory containing an existing library. Creation never intentionally overwrites an existing library. The welcome screen displays the suggested platform-default path.

Suggested locations (the displayed path is authoritative):

- macOS: `~/Library/Application Support/com.example.paper-library/default-library`
- Linux: `$XDG_DATA_HOME/com.example.paper-library/default-library`, falling back to `~/.local/share/com.example.paper-library/default-library`
- Windows: `%APPDATA%\com.example.paper-library\default-library`

Each portable library contains:

```text
library.sqlite3
covers/
```

The SQLite `documents` table stores JSON books, copies, patrons, loans and library settings. Covers are managed files referenced by relative paths. The default loan period belongs to the library. Interface language and the last-opened library path are separate application preferences. A missing last library produces a recovery screen, not a silently created replacement.

Use **Settings** to switch libraries. Close the app before moving the **complete directory**, then open it at the new location. Never move only the database or only the covers. A running library is locked against conflicting app instances. Do not use cloud synchronization on an open library.

### Backup and restore

Use **Settings → Create backup** to save a local ZIP archive containing a consistent SQLite snapshot, referenced covers, and a versioned checksum manifest. This captures committed data, including data in SQLite's WAL; do not use a raw copy of a live database file instead.

**Restore backup into a new directory** validates/extracts into a new or empty destination and offers to open it only after success. A restore does not replace the current library or interface-language preference. Keep the original library and archive until the restored library has been checked. For corruption or migration failures, preserve the original and use restore or select a different library rather than editing the database manually.

Libraries and backups are **not encrypted** by the application. Protect them using operating-system permissions, device encryption and secure backup storage. Patron/contact data must not be placed in public folders. The app uses local files and does not upload telemetry or library data.

## Catalog and lending

- Add/edit title, semicolon-separated authors, ISBN, publisher, year, genre, notes, DDC and LCC.
- Classifications are optional **text**, not numeric assignments or validated classifications. `005.133` and `QA76.73.R87` keep their notation, zeros, punctuation and internal spacing.
- Add copies with unique identifiers and optional shelf locations, acquisition dates and notes. A copy's identifier is text, so leading zeros remain intact.
- Register/edit patrons, then check out an available copy to an active patron. Default loan periods are 1–3650 calendar days (21 initially); the due date is checkout date plus that many days, and changing the default affects future checkouts only. Accept dates as `YYYY-MM-DD` and display them in the selected language.
- **Mark lost** does not close a loan. A lost borrowed copy remains in active/overdue/patron lists. **Record physical return** returns it to availability. **Resolve lost loan** requires a note, closes it without a return date, and leaves the copy lost.
- **Found / recover** restores a lost copy to checked out when it still has an active loan, or to available otherwise. Withdrawal is permanent and cannot be used with an active loan.
- Copies with loan history cannot be deleted. Books with copies cannot be deleted; deactivation preserves patron history.
- Lists are paginated at 50 records by default (25/50/100 options). Human-readable searches are literal substrings with Unicode case folding, accent-insensitivity, and collapsed whitespace; identifiers preserve their characters and leading zeros. ISBN lookup ignores spaces/hyphens; classification search is case-insensitive and retains punctuation. Sorting uses stable ID tie-breakers and puts missing values last in both directions.
- Filters include available books, copy status/location, active/completed/overdue loans and a selected patron. Checkout selectors search and page rather than loading every copy/patron.

### Covers and recovery

Book details allow adding, replacing or removing JPEG/PNG covers. Limits: 10 MiB encoded, 8,000 pixels per side, 128 MiB decoded. Images load lazily. A missing cover shows a repair message; replace/remove it without deleting the book.

New covers are staged before updating the book reference. Existing assets are retained until commit; an asset cleanup warning after commit does **not** mean the metadata transaction was rolled back. Errors show a localized summary and expandable technical details. Protect these details as local information when sharing screenshots.

## CSV book import

On **Books**, choose **Import books from CSV**. Import creates books only, with no physical copies, patrons, loans or images.

Samples: [comma-separated](examples/books.csv) and [pipe-separated](examples/books-pipe.csv).

- UTF-8 with optional BOM; comma (`,`) or pipe (`|`); LF/CRLF and a header row.
- Automatic detection considers quoted headers. If both/neither delimiter yields valid unique headers including `title`, select a delimiter explicitly. A title-only file may need manual selection.
- The preview shows the delimiter and renders at most 50 records per page. Use the delimiter selector to reparse. Input limits are **20 MiB** and **10,000 nonempty records**.

| Header | Required | Format |
| --- | --- | --- |
| `title` | Yes | Nonblank text |
| `isbn` | No | Text; leading zeros preserved |
| `authors` | No | Names separated by semicolons |
| `publisher` | No | Text |
| `publication_year` | No | Integer from 0 to 2147483647 |
| `genre` | No | Text |
| `ddc` | No | Text, e.g. `005.133` |
| `lcc` | No | Text, e.g. `QA76.73.R87` |
| `notes` | No | Text, including quoted multiline values |

Headers are case-sensitive and stay in English. Quote fields containing the chosen delimiter, quotes or line breaks; escape quotes as `""`. A pipe inside a comma-separated quoted field, or vice versa, remains data. Blank optional fields are absent, blank authors become an empty array, and entirely blank records are ignored. Record numbering includes header record 1; multiline values still count as one record.

Invalid UTF-8, headers, quotes, field counts, titles or years block import with record/column feedback. Unknown columns require explicit acknowledgement and are ignored. Possible duplicates are flagged by normalized ISBN or canonically Unicode-normalized, case-folded title/authors (author order ignored, accents preserved, repeated whitespace collapsed). Accent-insensitive search is **not** the duplicate identity rule. Approve duplicates explicitly to create separate records; nothing is merged or overwritten.

Preview writes nothing. Confirmation revalidates and rechecks duplicates in the same transaction used for saving. Cancellation is available before the database transaction starts; once it starts, the UI disables cancellation and reports the transaction's committed or rolled-back result. Changing library/input invalidates previews. After a successful import the count is announced and the catalog refreshed.

## Tests

```sh
cargo test --manifest-path src-tauri/Cargo.toml
npm test
npx playwright install chromium
npm run test:ui
npm run build
npm run tauri -- build --no-bundle
```

Browser tests use **mocked IPC** to isolate frontend navigation/forms, all three languages, import, keyboard focus, and automated accessibility checks. They are not desktop backend verification.

Real Tauri integration on Linux/Windows:

```sh
cargo install tauri-driver --locked
npm run tauri -- build --debug --no-bundle
npm run test:desktop
```

Linux needs `webkit2gtk-driver` (and `xvfb-run -a npm run test:desktop` on headless hosts). Windows needs an `msedgedriver` matching installed Microsoft Edge on `PATH`. The script starts/checks its own driver, uses a temporary app-data/library directory, then exercises real commands and UI. Native file selection is bypassed only by supplying test paths to real commands; book forms, copy/patron creation and checkout/return use UI. Do not run desktop tests against production data. The test data-directory override is debug-build only.

`tauri-driver` does not support macOS; manual native workflows and assistive-technology tests remain necessary. CI defines browser tests, Rust tests, both macOS architecture builds and Linux/Windows desktop jobs, but a committed workflow is **not** evidence of a successful run. See [VERIFICATION.md](VERIFICATION.md) for which tests actually ran, performance evidence and remaining platform/signing/accessibility checks.
