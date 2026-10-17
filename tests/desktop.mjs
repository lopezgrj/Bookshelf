import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

if (!["linux", "win32"].includes(process.platform)) {
  throw new Error("tauri-driver supports this test on Linux and Windows, not macOS. No desktop test was run.");
}
const root = await mkdtemp(path.join(tmpdir(), "paper-library-e2e-"));
const application = path.resolve(process.env.PAPER_LIBRARY_BINARY ||
  `src-tauri/target/debug/paper-library${process.platform === "win32" ? ".exe" : ""}`);
const port = Number(process.env.TAURI_DRIVER_PORT || 4444);
let driverError;
const driver = spawn("tauri-driver", ["--port", String(port)], {
  stdio: ["ignore", "inherit", "inherit"],
  env: { ...process.env, PAPER_LIBRARY_APP_DATA: path.join(root, "app-settings") },
});
driver.on("error", (error) => { driverError = error; });
const base = `http://127.0.0.1:${port}`;
let session;
async function request(route, body, method = "POST") {
  const response = await fetch(`${base}${route}`, {
    method, headers: { "Content-Type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(30_000),
  });
  const result = await response.json();
  if (!response.ok || result.value?.error) throw new Error(`${route}: ${JSON.stringify(result.value)}`);
  return result.value;
}
async function until(check, description, timeout = 15_000) {
  const start = Date.now();
  while (Date.now() - start < timeout) {
    if (driverError) throw driverError;
    if (await check()) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Timed out: ${description}`);
}
const command = (route, body, method) => request(`/session/${session}${route}`, body, method);
const js = (script, ...args) => command("/execute/sync", { script, args });
async function invoke(name, args = {}) {
  const result = await command("/execute/async", {
    script: `const done = arguments[arguments.length - 1];
      window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]).then(
        value => done({ ok: true, value }), error => done({ ok: false, error: String(error) }));`,
    args: [name, args],
  });
  assert.equal(result.ok, true, `${name}: ${result.error}`);
  return result.value;
}
async function element(selector) {
  const result = await command("/element", { using: "css selector", value: selector });
  return result["element-6066-11e4-a52e-4f735466cecf"];
}
async function click(selector) {
  await until(() => js("return !!document.querySelector(arguments[0]);", selector), selector);
  await command(`/element/${await element(selector)}/click`, {});
}
async function fill(selector, text) {
  const id = await element(selector);
  await command(`/element/${id}/clear`, {});
  await command(`/element/${id}/value`, { text });
}
const visible = (selector) => until(() => js(
  "return !!document.querySelector(arguments[0])?.checkVisibility();", selector), selector);

try {
  await until(async () => {
    try {
      const response = await fetch(`${base}/status`, { signal: AbortSignal.timeout(500) });
      return response.ok;
    } catch (error) {
      if (driver.exitCode !== null) throw new Error(`tauri-driver exited ${driver.exitCode}`, { cause: error });
      return false;
    }
  }, "responsive tauri-driver");
  const started = await request("/session", { capabilities: { alwaysMatch: { "tauri:options": { application } } } });
  session = started.sessionId;
  await command("/timeouts", { script: 30_000, implicit: 2_000 });
  await visible("[data-create-library]");
  const initial = await invoke("app_info");
  assert.equal(initial.testingMode, true, "Desktop tests require an isolated debug/e2e-test build");
  assert.equal(initial.libraryOpen, false, "Tests must not start with a user's library open");
  assert.ok(initial.appDataPath && path.relative(root, initial.appDataPath).split(path.sep)[0] !== ".."
    && !path.isAbsolute(path.relative(root, initial.appDataPath)), "Debug app-data isolation was not enabled; refusing to mutate preferences");
  const library = path.join(root, "library");
  await invoke("create_library", { path: library });
  await click('[data-page="books"]');
  await visible("#add-book");
  assert.equal(await js("return document.querySelector('#global-error').hidden"), true);
  await click("#add-book");
  await fill('dialog [name="title"]', "Français on the shelf");
  await fill('dialog [name="authors"]', "María Reader;François Pages");
  await fill('dialog [name="isbn"]', "0001234");
  await fill('dialog [name="ddc"]', "005.133");
  await fill('dialog [name="lcc"]', "QA76.73.R87");
  await click('dialog [type="submit"]');
  await visible(".book-entry");
  let books = await invoke("query_books", { query: { search: "francais", page: 1, pageSize: 50 } });
  assert.equal(books.total, 1);
  const book = books.items[0];
  assert.equal(book.ddc, "005.133");
  assert.equal(book.lcc, "QA76.73.R87");
  await click('.book-entry [data-book]');
  await click('dialog [data-edit]');
  await fill('dialog [name="publisher"]', "A physical press");
  await click('dialog [type="submit"]');
  await visible(".book-entry");
  const png = path.resolve("src-tauri/icons/icon.png");
  await invoke("set_cover", { bookId: book.id, sourcePath: png, operationId: "desktop-cover" });
  assert.match(await invoke("cover_data", { bookId: book.id }), /^data:image\/png;base64,/);
  await click('.book-entry [data-book]');
  await click("dialog [data-add-copy]");
  await fill('dialog [name="copyIdentifier"]', "00001");
  await fill('dialog [name="location"]', "Étage 2 / A");
  await click('dialog [type="submit"]');
  await visible(".book-entry");
  await click('[data-page="patrons"]');
  await click("#add-patron");
  await fill('dialog [name="name"]', "Alex Reader");
  await fill('dialog [name="patronIdentifier"]', "0001");
  await click('dialog [type="submit"]');
  await visible('[data-patron]');
  await click('[data-page="loans"]');
  await click("#checkout");
  await click('dialog [data-copy-picker]');
  await click('dialog .picker [data-select]');
  await click('dialog [data-patron-picker]');
  await click('dialog .picker [data-select]');
  await click('dialog [type="submit"]');
  await visible('[data-return]');
  await click("[data-return]");
  await click('dialog.confirmation [data-yes]');
  await until(async () => {
    const result = await invoke("query_loans", { query: { status: "completed", page: 1, pageSize: 50 } });
    return result.total === 1 && result.items[0].loan.closureReason === "returned";
  }, "committed return");
  for (const delimiter of [",", "|"]) {
    const csv = path.join(root, delimiter === "," ? "comma.csv" : "pipe.csv");
    await writeFile(csv, `title${delimiter}ddc${delimiter}lcc\nImported ${delimiter === "," ? "comma" : "pipe"}${delimiter}001.2${delimiter}QA1\n`);
    const preview = await invoke("preview_book_csv", { path: csv, delimiter: null, operationId: `preview-${delimiter}` });
    assert.equal(preview.errors.length, 0);
    assert.equal(preview.delimiter, delimiter);
    assert.equal(await invoke("import_book_csv", {
      token: preview.token, allowDuplicates: false, ignoreUnknownColumns: false, operationId: `import-${delimiter}`,
    }), 1);
  }
  const archive = path.join(root, "backup.zip");
  await invoke("backup_library", { path: archive, operationId: "desktop-backup" });
  const restored = path.join(root, "restored");
  await invoke("restore_library", { path: archive, destination: restored, operationId: "desktop-restore" });
  await invoke("open_library", { path: restored });
  books = await invoke("query_books", { query: { page: 1, pageSize: 50 } });
  assert.equal(books.total, 3);
  assert.equal(books.items.find((item) => item.id === book.id).publisher, "A physical press");
  await click('[data-page="books"]');
  await visible(".book-entry img");
  assert.equal(await js("return document.querySelector('#global-error').hidden"), true);
  console.log("PASS: real Tauri startup, book edit/search/classifications/cover, copy/patron, checkout/return, both CSV delimiters, backup/restore.");
} finally {
  try {
    if (session) await command("", undefined, "DELETE");
  } finally {
    if (driver.exitCode === null && driver.pid) {
      const stopped = new Promise((resolve) => driver.once("exit", resolve));
      driver.kill();
      await stopped;
    }
    await rm(root, { recursive: true, force: true });
  }
}
