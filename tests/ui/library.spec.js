import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { messages } from "../../src/locales.js";
import { csvMessages } from "../../src/csv-locales.js";

async function prepare(page, language = "en", libraryOpen = true) {
  await page.addInitScript(({ language, libraryOpen }) => {
    let selectedLanguage = language;
    const books = Array.from({ length: 121 }, (_, index) => ({
      id: `book-${index}`, title: index ? `Book ${String(index).padStart(3, "0")}` : "Français en bibliothèque",
      authors: ["María Reader"], ddc: "005.133", lcc: "QA76.73.R87", totalCopies: 1, availableCopies: 1,
    }));
    const patrons = [{ id: "p1", name: "François Pages", patronIdentifier: "0001", active: true }];
    const copies = [{ id: "c1", bookId: "book-0", copyIdentifier: "00001", status: "available", bookTitle: books[0].title, location: "Étage 2" }];
    const loans = [{ loan: { id: "l1", dueDate: "2025-01-03", closedAt: null }, bookTitle: books[0].title,
      patronName: patrons[0].name, copyIdentifier: "00001", copyStatus: "lost", overdue: true }];
    window.testCalls = [];
    window.testErrors = {};
    window.testFile = "/tmp/books.csv";
    window.testDelays = {};
    window.testOperation = { phase: "parsing", completed: 0, total: 121, cancellable: true };
    window.testPreview = {
      token: "preview-1", delimiter: "|", errors: [], unknownColumns: [],
      rows: Array.from({ length: 121 }, (_, index) => ({ record: index + 2, duplicate: index === 0,
        book: { title: `Imported ${index}`, authors: ["François"], ddc: "005.133", lcc: "QA76.73.R87" } })),
    };
    const normalize = (s) => String(s).normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
      transformCallback: () => 1,
      invoke: async (command, args = {}) => {
        window.testCalls.push({ command, args });
        if (window.testDelays[command]) {
          await new Promise((resolve) => setTimeout(resolve, window.testDelays[command]));
          if (window.testCancelled === args.operationId) throw "CSV_CANCELLED: No changes saved";
        }
        if (window.testErrors[command]) throw window.testErrors[command];
        if (command === "app_info") return { settings: { language: selectedLanguage, loanPeriodDays: 21 }, libraryOpen,
          libraryPath: "/tmp/test-library", defaultLibraryPath: "/tmp/PaperLibrary" };
        if (command === "update_settings") { selectedLanguage = args.language; return; }
        if (command === "plugin:dialog|open" || command === "plugin:dialog|save") return window.testFile;
        if (command === "plugin:window|set_title" || command === "plugin:window|close") return;
        if (command === "plugin:event|listen") return 1;
        if (command.startsWith("query_")) {
          const query = args.query || {};
          let items = { query_books: books, query_patrons: patrons, query_copies: copies, query_loans: loans }[command];
          if (!items) throw new Error(`Unexpected query: ${command}`);
          if (query.search) items = items.filter((item) => normalize(JSON.stringify(item)).includes(normalize(query.search)));
          const total = items.length;
          const pageSize = query.pageSize || 50;
          const currentPage = query.page || 1;
          return { items: items.slice((currentPage - 1) * pageSize, currentPage * pageSize), total, page: currentPage, pageSize };
        }
        if (command === "save_book") { books.push({ ...args, id: args.id || "new-book", totalCopies: 0, availableCopies: 0 }); return; }
        if (command === "save_patron" || command === "add_copy" || command === "update_copy" || command === "checkout") return;
        if (command === "delete_book") { books.splice(books.findIndex((book) => book.id === args.id), 1); return; }
        if (command === "resolve_lost_loan") { loans[0].loan.closedAt = "2025-01-05"; loans[0].loan.closureReason = "lost_resolved"; return; }
        if (command === "return_loan") { loans[0].loan.closedAt = "2025-01-05"; loans[0].loan.closureReason = "returned"; return; }
        if (command === "preview_book_csv") return structuredClone(window.testPreview);
        if (command === "import_book_csv") return window.testPreview.rows.length;
        if (command === "cancel_book_csv") return;
        if (command === "cancel_operation") { window.testCancelled = args.operationId; return; }
        if (command === "operation_status") return window.testOperation;
        if (command === "create_library" || command === "open_library") { libraryOpen = true; return; }
        throw new Error(`Unexpected command: ${command}`);
      },
    };
  }, { language, libraryOpen });
  await page.goto("/");
  await expect(page.locator("main h1")).toBeVisible();
}

for (const language of ["en", "es", "fr"]) {
  test(`${language}: navigation, classification form, validation, search and language`, async ({ page }) => {
    const t = messages[language];
    await prepare(page, language);
    await expect(page.locator("html")).toHaveAttribute("lang", language);
    await expect(page.locator(".book-card")).toHaveCount(50);
    await page.getByRole("button", { name: t.addBook, exact: true }).click();
    const dialog = page.getByRole("dialog");
    await page.getByRole("button", { name: t.save, exact: true }).click();
    await expect(dialog.locator('[name="title"]')).toHaveAttribute("aria-invalid", "true");
    await expect(dialog.locator("[data-form-error]")).toBeFocused();
    await dialog.getByLabel(t.title, { exact: true }).fill("A test book");
    await dialog.getByLabel(t.ddc, { exact: true }).fill("005.133");
    await dialog.getByLabel(t.lcc, { exact: true }).fill("QA76.73.R87");
    await dialog.getByRole("button", { name: t.save, exact: true }).click();
    await expect(dialog).not.toBeVisible();
    const saved = await page.evaluate(() => window.testCalls.find((call) => call.command === "save_book").args);
    expect(saved.ddc).toBe("005.133");
    expect(saved.lcc).toBe("QA76.73.R87");
    await page.locator('#filters [name="search"]').fill("francais");
    await page.getByRole("button", { name: t.search, exact: true }).click();
    await expect(page.locator(".book-card")).toHaveCount(1);
    await page.locator(`[data-page="patrons"]`).click();
    await expect(page.getByText("François Pages", { exact: true })).toBeVisible();
    await page.locator(`[data-page="loans"]`).click();
    await expect(page.getByRole("button", { name: t.resolveLost, exact: true })).toBeVisible();
    const expectedDate = new Intl.DateTimeFormat({ en: "en-US", es: "es-ES", fr: "fr-FR" }[language],
      { dateStyle: "medium" }).format(new Date("2025-01-03T12:00:00"));
    await expect(page.getByRole("cell", { name: expectedDate, exact: true })).toBeVisible();
    await page.locator(`[data-page="settings"]`).click();
    const next = language === "fr" ? "en" : "fr";
    await page.locator('[name="language"]').selectOption(next);
    await page.getByRole("button", { name: t.save, exact: true }).click();
    await expect(page.locator("html")).toHaveAttribute("lang", next);
  });

  test(`${language}: pipe import is bounded, requires duplicate approval and can cancel`, async ({ page }) => {
    await prepare(page, language);
    const t = csvMessages[language];
    await page.getByRole("button", { name: t.import, exact: true }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.locator("tbody tr")).toHaveCount(50);
    await expect(dialog.getByRole("button", { name: t.confirm, exact: true })).toBeDisabled();
    await expect(dialog.locator("tbody").getByText("005.133", { exact: true }).first()).toBeVisible();
    await dialog.getByLabel(t.confirmDuplicates).check();
    await dialog.getByRole("button", { name: t.next, exact: true }).click();
    await expect(dialog.locator("tbody tr")).toHaveCount(50);
    await expect(dialog.getByLabel(t.confirmDuplicates)).toBeChecked();
    await dialog.locator("#csv-delimiter").selectOption("|");
    await expect(dialog.getByLabel(t.confirmDuplicates)).not.toBeChecked();
    await dialog.getByLabel(t.confirmDuplicates).check();
    await dialog.getByRole("button", { name: t.confirm, exact: true }).click();
    await expect(dialog).not.toBeVisible();
    await expect(page.locator("#toast")).toContainText("121");
    await page.getByRole("button", { name: t.import, exact: true }).click();
    await page.getByRole("dialog").getByRole("button", { name: t.cancel, exact: true }).click();
    await expect(page.getByRole("dialog")).not.toBeVisible();
    expect(await page.evaluate(() => window.testCalls.filter((call) => call.command === "import_book_csv").length)).toBe(1);
  });
}

test("pagination resets search and errors are actionable without losing form values", async ({ page }) => {
  await prepare(page);
  await page.locator('[name="pageSize"]').selectOption("25");
  await expect(page.locator(".book-card")).toHaveCount(25);
  await page.getByRole("button", { name: "Next", exact: true }).click();
  await expect(page.locator(".book-card h2").first()).toHaveText("Book 025");
  await page.locator('#filters [name="search"]').fill("Book 001");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await expect(page.locator(".book-card h2").first()).toHaveText("Book 001");
  await page.evaluate(() => { window.testErrors.save_book = "SQLITE_BUSY: Another writer is using the library."; });
  await page.getByRole("button", { name: "Add book", exact: true }).click();
  await page.getByRole("dialog").getByLabel("Title", { exact: true }).fill("Unsaved");
  await page.getByRole("dialog").getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByRole("dialog").locator("[data-form-error]")).toBeVisible();
  await expect(page.getByRole("dialog").getByLabel("Title", { exact: true })).toHaveValue("Unsaved");
  await page.getByRole("dialog").getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Confirm this action?" })).toBeVisible();
  await page.getByRole("dialog", { name: "Confirm this action?" }).getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByRole("dialog")).not.toBeVisible();
});

test("destructive action is confirmed and backend error is visible in the open dialog", async ({ page }) => {
  await prepare(page);
  await page.locator(".book-card").first().getByRole("button", { name: "Edit", exact: true }).click();
  await page.getByRole("button", { name: "Delete book", exact: true }).click();
  await page.getByRole("dialog", { name: "Confirm this action?" }).getByRole("button", { name: "Cancel", exact: true }).click();
  expect(await page.evaluate(() => window.testCalls.filter((call) => call.command === "delete_book").length)).toBe(0);
  await page.evaluate(() => { window.testErrors.delete_book = "BOOK_HAS_COPIES: Remove copies first."; });
  await page.getByRole("button", { name: "Delete book", exact: true }).click();
  await page.getByRole("dialog", { name: "Confirm this action?" }).getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.locator("dialog [data-form-error]")).toBeVisible();
});

test("loss resolution records a required note instead of a physical return", async ({ page }) => {
  await prepare(page);
  await page.locator('[data-page="loans"]').click();
  await page.getByRole("button", { name: "Resolve lost loan", exact: true }).click();
  await page.getByRole("dialog").getByLabel("Required loss resolution note").fill("Replacement accepted");
  await page.getByRole("dialog").getByRole("button", { name: "Save", exact: true }).click();
  await page.getByRole("dialog", { name: "Confirm this action?" }).getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByRole("cell", { name: "Loss resolved (not returned)", exact: true })).toBeVisible();
  expect(await page.evaluate(() => window.testCalls.some((call) => call.command === "return_loan"))).toBe(false);
});

test("keyboard focus restores, unsaved checkout survives child pickers, and AA automated checks pass", async ({ page }) => {
  await prepare(page);
  expect((await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze()).violations).toEqual([]);
  await page.locator('[data-page="loans"]').click();
  const checkout = page.getByRole("button", { name: "Check out", exact: true });
  await checkout.click();
  await page.getByRole("button", { name: "Search / select an available copy", exact: true }).click();
  await page.locator("[data-select]").click();
  await page.getByRole("dialog").getByRole("button", { name: "Search / select a patron", exact: true }).click();
  await page.locator("[data-select]").click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog", { name: "Confirm this action?" })).toBeVisible();
  await page.getByRole("dialog", { name: "Confirm this action?" }).getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(checkout).toBeFocused();
  await page.locator('[data-page="books"]').click();
  await page.getByRole("button", { name: "Add book", exact: true }).click();
  expect((await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze()).violations).toEqual([]);
  await page.getByRole("dialog").getByRole("button", { name: "Close dialog", exact: true }).click();
  await page.locator("html").evaluate((node) => { node.style.fontSize = "200%"; });
  await expect(page.getByRole("button", { name: "Add book", exact: true })).toBeInViewport();
});

test("first launch does not silently create a library", async ({ page }) => {
  await prepare(page, "en", false);
  await expect(page.getByRole("button", { name: "Create library", exact: true })).toBeVisible();
  expect(await page.evaluate(() => window.testCalls.some((call) => call.command === "create_library"))).toBe(false);
});

test("a native startup capability failure is reported even before the shell loads", async ({ page }) => {
  await page.addInitScript(() => {
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
      transformCallback: () => 1,
      invoke: async () => { throw new Error("CAPABILITY_DENIED: Native startup failed"); },
    };
  });
  await page.goto("/");
  await expect(page.locator("#global-error")).toBeVisible();
  await page.getByText("Technical details", { exact: true }).click();
  await expect(page.locator("#global-error")).toContainText("CAPABILITY_DENIED");
});

test("CSV errors and unknown columns block confirmation; delimiter override reparses", async ({ page }) => {
  await prepare(page, "fr");
  const t = csvMessages.fr;
  await page.evaluate(() => {
    window.testPreview.errors = [{ record: 2, column: "publication_year", code: "invalid_year" }];
    window.testPreview.unknownColumns = ["shelf"];
    window.testPreview.delimiter = null;
  });

  await page.getByRole("button", { name: t.import, exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText(t.invalid_year, { exact: false })).toBeVisible();
  await expect(dialog.getByRole("button", { name: t.confirm, exact: true })).toBeDisabled();
  await page.evaluate(() => { window.testPreview.errors = []; window.testPreview.delimiter = ","; });
  await dialog.locator("#csv-delimiter").selectOption(",");
  await expect(dialog.getByText(t.invalid_year, { exact: false })).not.toBeVisible();
  await dialog.getByLabel(t.confirmDuplicates).check();
  await expect(dialog.getByRole("button", { name: t.confirm, exact: true })).toBeDisabled();
  await dialog.getByLabel(t.ignore).check();
  await expect(dialog.getByRole("button", { name: t.confirm, exact: true })).toBeEnabled();
  await dialog.getByRole("button", { name: t.cancel, exact: true }).click();
  expect(await page.evaluate(() => window.testCalls.some((call) => call.command === "import_book_csv"))).toBe(false);
});
test("CSV progress stays responsive, cancellation is requested, final commit cannot be cancelled", async ({ page }) => {
  await prepare(page);
  await page.evaluate(() => { window.testDelays.preview_book_csv = 900; });
  await page.locator("#import-books").click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.locator("progress")).toHaveAttribute("value", "0");
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog.locator("[data-progress]")).toContainText("Cancellation requested");
  await expect(dialog.locator("[data-error]")).toContainText("Cancelled; no books imported.");
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await page.evaluate(() => {
    window.testDelays.preview_book_csv = 0;
    window.testDelays.import_book_csv = 1200;
    window.testOperation = { phase: "committing", completed: 121, total: 121, cancellable: false };
  });
  await page.locator("#import-books").click();
  await dialog.getByLabel(csvMessages.en.confirmDuplicates).check();
  await dialog.getByRole("button", { name: "Import books", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Cancel", exact: true })).toBeDisabled();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeVisible();
  await expect(dialog).not.toBeVisible({ timeout: 3000 });
  await expect(page.locator("#toast")).toContainText("121");
});

test("settings changes require discard confirmation before switching pages", async ({ page }) => {
  await prepare(page);
  await page.locator('[data-page="settings"]').click();
  await expect(page.locator('[name="loanPeriodDays"]')).toHaveAttribute("max", "3650");
  await page.locator('[name="loanPeriodDays"]').fill("30");
  await page.locator('[data-page="books"]').click();
  await page.getByRole("dialog", { name: "Confirm this action?" }).getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.locator('[name="loanPeriodDays"]')).toHaveValue("30");
  await page.locator('[data-page="books"]').click();
  await page.getByRole("dialog", { name: "Confirm this action?" }).getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.locator("#add-book")).toBeVisible();
});

test("copy search, location, sort and page-size controls submit their own non-nested form", async ({ page }) => {
  await prepare(page);
  await page.locator(".book-card").first().getByRole("button", { name: "Edit", exact: true }).click();
  const form = page.locator("dialog .copy-filters");
  expect(await form.evaluate((node) => node.parentElement.closest("form") === null)).toBe(true);
  await form.locator('[name="search"]').fill("00001");
  await form.locator('[name="location"]').fill("etage");
  await form.locator('[name="pageSize"]').selectOption("25");
  await form.locator('[name="descending"]').check();
  await form.getByRole("button", { name: "Search", exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.testCalls.filter((call) => call.command === "query_copies").at(-1).args.query))
    .toMatchObject({ search: "00001", location: "etage", pageSize: 25, page: 1, sort: "location", descending: true });
});

test("invalid CSV preview errors are paginated instead of rendering the entire error list", async ({ page }) => {
  await prepare(page);
  await page.evaluate(() => {
    window.testPreview.rows = [];
    window.testPreview.errors = Array.from({ length: 101 }, (_, index) => ({
      record: index + 2, column: "title", code: "required_title",
    }));
  });
  await page.locator("#import-books").click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.locator(".csv-errors li")).toHaveCount(50);
  await dialog.getByRole("button", { name: "Next", exact: true }).click();
  await dialog.getByRole("button", { name: "Next", exact: true }).click();
  await expect(dialog.locator(".csv-errors li")).toHaveCount(1);
  await expect(dialog.locator(".csv-errors li")).toContainText("102");
  await expect(dialog.getByRole("button", { name: "Import books", exact: true })).toBeDisabled();
});
