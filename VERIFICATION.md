# Implementation and release verification

## Scope and tested source

This report accompanies the expanded [specification](SPECIFICATION.md). **This is not release approval.** Functional implementation, executed tests, unexecuted tests, and platform distribution requirements are separate.

- Execution date: 2026-10-15 (UTC).
- Source: working-directory snapshot, not a Git commit (workspace has no Git repository).
- Source SHA-256: `285065a8bfdecc9f12e5670f64481b20d875e37824722441f7078427310d562e` (33 files). Includes all files recursively in `src`, `tests`, `.github`, `src-tauri/src`, `src-tauri/capabilities`, `src-tauri/icons`, `examples`, plus non-Markdown files directly in the project root and `src-tauri`. Hash input is sorted relative path, NUL, file bytes, NUL for each file; generated dependencies/build outputs are excluded.
- Production binary SHA-256: `7d1e10de30178f3c25375e442a7a2f0f946df7df1d9c0d567ef8c3cf478dfbfd`.
- Debug binary SHA-256: `0688685f5c84db0c2982b3115919b67c945642489f42b3531fa21bafa11327f3`.
- Development DMG SHA-256: `2d223775caa06007bb711906d14009dfc8ab52ea8496ca26bad3148b8ceb3390`.
- Host: macOS 27.0.1 (26A434), arm64, 20 physical cores, 128 GiB RAM. Local storage/cache conditions were not characterized for release performance acceptance.
- Rust/Cargo 1.99.0 (Homebrew); Node 26.11.1; npm 11.20.0; Vite 6.4.4; Rust Tauri 2.12.4; dialog plugin 2.8.3.
- Browser regression runner: Playwright Chromium; this is not the native WKWebView.
- Native WKWebView exact version and minimum-baseline OS behavior have not been recorded.

## Executed checks

| Check | Result | Limits |
| --- | --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml` | Passed: 30 tests, 1 ignored | Temporary SQLite/domain/archive/CSV tests; large fixture excluded by default |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --check` | Passed | Formatting only |
| `npm test` | Passed: 4 tests | Translation completeness and interpolation; not native rendering |
| `npm run test:ui -- --workers=3` | Passed: 17 tests | Mocked IPC: localized forms/navigation/dates/errors, classifications, pagination, CSV approval/limits on rendered rows and errors, progress/cancel/commit phase, dialog focus, unsaved edits, loss resolution, copy filters |
| axe WCAG A/AA checks in UI suite | Passed for tested catalog/form states | Does not establish WCAG 2.2 AA conformance; manual screen readers still required |
| `npm run tauri -- build --debug --no-bundle` | Passed, macOS arm64 | Unsigned debug binary |
| `npm run tauri -- build --no-bundle` | Passed, macOS arm64 | Unsigned production binary; not a DMG/installer |
| `npm run tauri -- build --bundles dmg` | Passed, macOS arm64 | Development DMG produced, 4.44 MiB; not Developer ID signed or notarized |
| `hdiutil verify` plus read-only DMG inspection | Passed | Valid image checksum; app contains arm64 executable; `LSMinimumSystemVersion=13.0`. `codesign` reports linker ad-hoc signature, no team identifier or sealed resources; not a distribution signature. Temporary volume unmounted |
| Isolated native startup launch | Passed for first-launch welcome | Final debug process stayed alive; CoreGraphics reported on-screen `Paper Library` window at 1180×820. Window-scoped screenshot verified rendered navigation and create/open/restore welcome actions, isolated directory path, and no visible startup error. Existing-library catalog/full workflows were not verified |
| macOS automated native UI interaction | Not run | System Events denied assistive access (`-25211`); no permission changes attempted |
| Real desktop end-to-end script | Not run locally | `tauri-driver` script targets Linux/Windows, not macOS |

The browser tests caught and fixed two startup/dialog regressions: errors before the shell existed, and Escape incorrectly requesting cancellation during final CSV commit. They also verify nested picker closure does not erase the parent checkout's unsaved state.

## Traceability

`partial` means at least one required behavior, coverage item, or platform verification remains outstanding. A `passed` verification status below applies only to the explicitly named local evidence, **not** an entire cross-platform release.

| Requirement | Implementation | Verification | Evidence and remaining work |
| --- | --- | --- | --- |
| CAT-001 | implemented | passed | Classification string round-trip/pipe CSV, paginated forms, location controls; native full-workflow coverage still required |
| CIRC-001 | implemented | passed | `checkout_and_return_update_json_documents_together`, `lost_checkout_resolution_recovery_and_withdrawal_follow_valid_transitions`, overdue boundary test; mocked UI loss resolution |
| PAT-001 | implemented | passed | Patron persistence/eligibility exercised by lending tests; localized patron UI tests; native workflows pending |
| I18N-001 | partial | passed | All label keys in three languages; localized dates/forms/CSV/errors tested. Generic localized error summaries retain backend technical details; full domain-specific actionable localization remains to audit |
| UI-001 | implemented | passed | Mocked browser workflows, confirmations, loading/errors, focus, dirty-form protection; native workflows pending |
| COVER-001 | partial | passed | `cover_replacement_valid_corrupt_oversized_and_failed_write_preserve_old_asset`, managed-path and archive cover tests. Complete failure-injection/crash-cleanup matrix not yet established |
| CSV-001 | implemented | passed | Comma/pipe/BOM/quotes/headers/unknown columns/duplicates/rollback/limits; frontend preview bounded to 50 rows and cancellation phase tested; native picker workflow pending |
| SEARCH-001 | implemented | passed | Full Unicode case-folding (ß/ss, sigma, ligatures), accent-vs-identity, copy picker book title/author/location search and pagination tests; exhaustive sort/null/filter combinations need broader fixtures |
| STORE-001 | implemented | passed | `portable_library_move_reopen_settings_and_failed_locked_switch_preserve_selection`; platform movement matrix pending |
| BACKUP-001 | implemented | passed | `backup_round_trip_captures_wal_and_referenced_cover_assets`, `rejects_traversal_symlink_and_checksum_archives_without_touching_active_library`; platform failure matrix pending |
| DATA-001 | partial | passed | Real SQLite JSON tests, uniqueness, lifecycle transactions, version-one migration; exhaustive unsupported-document/corruption/write-failure coverage pending |
| TEST-001 | partial | not run | Unit/browser suites executed; real native full workflows, OS matrix, required failure injections and manual tests remain incomplete |
| A11Y-001 | partial | not run | Native dialogs, labels, focus return/traps, live status, 200% text and automated axe checks; VoiceOver/NVDA/Orca and full WCAG 2.2 AA review not run |
| PLATFORM-001 | partial | not run | macOS arm64 production/debug builds; CI matrix defined but not executed here. Minimum OS/Intel/Windows/Linux installers and signing/notarization unverified |
| PERF-001 | partial | not run | Deterministic backend fixture provided; full production UI/startup/CSV threshold protocol not run |
| RECOVERY-001 | partial | passed | Pre-migration snapshot, bounded contention, staging/asset checks, explicit errors and local-only data. Full disk/permissions/crash/cleanup failure matrix incomplete |
| TRACE-001 | implemented | passed | This report separates implementation, local checks, blockers and platform requirements |

## Performance evidence

The ignored Rust test `backend_fixture_query_metrics_10k_books_20k_copies_5k_patrons_50k_loans` creates a deterministic large fixture. Run it explicitly:

```sh
cargo test --release --manifest-path src-tauri/Cargo.toml \
  backend_fixture_query_metrics_10k_books_20k_copies_5k_patrons_50k_loans \
  -- --ignored --nocapture
```

Final optimized-profile run on the host above:

```text
fixture=10k books/20k copies/5k patrons/50k loans/1k active/1k covers
p95=38.648458ms, max=39.407042ms
1 passed; fixture run elapsed 6.75s (compilation excluded)
```

This is **backend-only release-profile timing**, not production UI latency. It does not establish the 500 ms search-to-render target. An earlier pre-full-casefold debug/test-profile implementation run reported p95 351.6 ms; it is historical evidence, not a result for the final source snapshot. Fixture covers are test assets, not established representative image sizes.

Still required: 20 production warm startup measurements plus cold launch after reboot; 100 search/filter/page UI measurements; five 10,000-record preview/commit runs per delimiter against the required catalog; cover responsiveness and cancellation under load; recorded fixture image sizes, storage/cache and hardware details. No fabricated startup/CSV percentile or pass claim is made.

## Platform and distribution blockers

| Target | Build | Native workflows/install | Signing |
| --- | --- | --- | --- |
| macOS arm64 | Binary and DMG passed on current host | Native welcome rendered; DMG checksum/contents verified read-only. macOS 13 installation, existing-library catalog and full workflow checks pending | Ad-hoc linker signature only; no Developer ID signing/notarization |
| macOS x86_64 | Not run | Not run | Not signed/notarized |
| Windows 11 x86_64 | Not run | Not run; CI job provided, including matching WebView driver setup | Installer signing unavailable |
| Ubuntu 24.04 x86_64 | Not run | Not run; CI job provided with WebKitGTK/Xvfb dependencies | Package verification not run |

CI definitions are reproducible test instructions, not proof of successful CI execution. Platform-baseline and current-version testing, package installation/upgrade, WebView prerequisites, backup portability across operating systems, assistive technology, and distribution credentials remain release gates.

## Native test isolation

[tests/desktop.mjs](tests/desktop.mjs) launches its own driver with a temporary debug app-data directory, verifies the backend reports that isolated path before mutating preferences, then uses the real command layer for book/copy/patron/lending/CSV/backup/restore operations. UI forms drive book editing, copy/patron creation and checkout/return. Native file pickers are not automated; test paths are supplied to real Rust commands. Temporary test files are removed after driver shutdown.

Debug-only `PAPER_LIBRARY_APP_DATA` isolates startup preferences. It must not be interpreted as a supported production setting or used to claim native test success on macOS.
