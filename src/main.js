import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { importBooks } from "./csv-import.js";
import { csvText } from "./csv-locales.js";
import { translate } from "./locales.js";
import { watchOperation } from "./operation-progress.js";
import "./style.css";

let language = "en";
let page = "books";
let info;
let requestNumber = 0;
let coverObserver;
const dirtyDialogs = new Set();
let fieldNumber = 0;
let operation;
let closingWindow = false;
let settingsDirty = false;
let csvState = { open: false, busy: false };
const hasUnsaved = () => settingsDirty || dirtyDialogs.size > 0 || csvState.open;
const query = { search: "", page: 1, pageSize: 50, sort: "title", descending: false };
const app = document.querySelector("#app");
const t = (key) => translate(language, key);
const escape = (value) => String(value ?? "").replace(/[&<>"']/g, (ch) => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
}[ch]));
const statusText = (value) => t(value);
const date = (value) => value ? new Intl.DateTimeFormat({ en: "en-US", es: "es-ES", fr: "fr-FR" }[language],
  { dateStyle: "medium" }).format(new Date(`${value}T12:00:00`)) : "—";

function toast(message, error = false) {
  const node = document.querySelector("#toast");
  node.textContent = message;
  node.className = `toast visible${error ? " error" : ""}`;
  node.setAttribute("role", error ? "alert" : "status");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => node.classList.remove("visible"), 7000);
}

function errorMessage(error) {
  const raw = String(error);
  const code = raw.split(":")[0];
  const known = {
    COMMITTED_CLEANUP_PENDING: "committed",
    INVALID_IMAGE: "coverHint", INVALID_IMAGE_SIZE: "coverHint",
    COVER_MISSING: "invalidCoverAsset", COVER_INVALID: "invalidCoverAsset",
    NO_LIBRARY: "welcome", LIBRARY_NOT_FOUND: "missingLibrary",
    LIBRARY_LOCKED: "libraryLocked", SQLITE_BUSY: "databaseBusy",
    BOOK_HAS_COPIES: "bookHasCopies", COPY_HAS_HISTORY: "copyHasHistory",
    COPY_HAS_LOANS: "copyHasHistory", COPY_HAS_ACTIVE_LOAN: "copyHasActiveLoan",
    INACTIVE_PATRON: "inactivePatron", INVALID_DATE: "invalidDate",
    DUPLICATE_IDENTIFIER: "duplicateIdentifier", OPERATION_CANCELLED: "cancelled",
    OPERATION_COMMITTING: "committing", INVALID_LOAN_PERIOD: "loanPeriodInvalid",
  };
  return known[code] ? t(known[code]) : t("error");
}

function showError(error) {
  if (!document.querySelector("#global-error")) shell();
  const activeDialog = [...document.querySelectorAll("dialog[open]")].at(-1);
  const output = activeDialog?.querySelector("[data-form-error]") || document.querySelector("#global-error");
  if (!output) { toast(errorMessage(error), true); return; }
  output.hidden = false;
  output.innerHTML = `<p>${escape(errorMessage(error))}</p><details><summary>${t("details")}</summary><pre>${escape(String(error))}</pre></details>`;
}

async function act(work) {
  try { return await work(); }
  catch (error) { showError(error); return undefined; }
}

function button(label, attributes = "", style = "secondary") {
  return `<button type="button" class="button ${style}" ${attributes}>${escape(label)}</button>`;
}

function field(key, value = "", attributes = "") {
  const id = `field-${key}-${++fieldNumber}`;
  return `<label for="${id}">${key === "isbn" ? "ISBN" : t(key)}</label>
    <input id="${id}" name="${key}" value="${escape(value)}" ${attributes}>`;
}

function textarea(key, value = "", attributes = "") {
  const id = `field-${key}-${++fieldNumber}`;
  return `<label for="${id}">${t(key)}</label><textarea id="${id}" name="${key}" ${attributes}>${escape(value)}</textarea>`;
}

function select(key, options, value, attributes = "") {
  const fieldId = `field-${key}-${++fieldNumber}`;
  return `<label for="${fieldId}">${t(key)}</label><select id="${fieldId}" name="${key}" ${attributes}>${options.map(([id, label]) =>
    `<option value="${escape(id)}" ${String(value) === String(id) ? "selected" : ""}>${escape(label)}</option>`).join("")}</select>`;
}

async function modal(title, content, submit, { dismissible = true } = {}) {
  const opener = document.activeElement;
  const dialog = document.createElement("dialog");
  const titleId = `dialog-title-${++fieldNumber}`;
  const wrapper = submit ? "form" : "div";
  dialog.className = "app-dialog";
  dialog.innerHTML = `<${wrapper} ${submit ? "novalidate" : ""}><div class="dialog-heading"><h2 id="${titleId}">${escape(title)}</h2>${button(t("closeDialog"), "data-close")}</div>
    <div data-form-error role="alert" tabindex="-1" hidden></div>${content}
    <div class="modal-actions">${button(t("cancel"), "data-close")}${submit ? `<button class="button primary" type="submit">${t("save")}</button>` : ""}</div></${wrapper}>`;
  dialog.setAttribute("aria-labelledby", titleId);
  let changed = false;
  let saving = false;
  const markDirty = () => { if (submit) { changed = true; dirtyDialogs.add(dialog); } };
  dialog.addEventListener("input", markDirty);
  const close = async (force = false) => {
    if (!force && (!dismissible || saving)) return;
    if (changed && !force && !await confirmAction(t("discard"))) return;
    dialog.close();
    dialog.remove();
    dirtyDialogs.delete(dialog);
    if (opener?.isConnected) opener.focus();
  };
  dialog.querySelectorAll("[data-close]").forEach((node) => node.addEventListener("click", () => act(() => close())));
  dialog.addEventListener("cancel", (event) => { event.preventDefault(); void act(() => close()); });
  if (submit) dialog.querySelector("form").addEventListener("submit", (event) => {
    event.preventDefault();
    void act(async () => {
      const form = event.currentTarget;
      const invalid = [...form.elements].filter((element) => element.willValidate && !element.checkValidity());
      form.querySelectorAll("[aria-invalid]").forEach((element) => element.removeAttribute("aria-invalid"));
      if (invalid.length) {
        const output = form.querySelector("[data-form-error]");
        output.hidden = false;
        output.textContent = t("validation");
        invalid.forEach((element) => element.setAttribute("aria-invalid", "true"));
        output.focus();
        invalid[0].setAttribute("aria-describedby", "validation-error");
        output.id = "validation-error";
        return;
      }
      const submitButton = form.querySelector('[type="submit"]');
      if (saving) return;
      saving = true;
      submitButton.disabled = true;
      try {
        await submit(Object.fromEntries(new FormData(form)));
      } catch (error) {
        const output = form.querySelector("[data-form-error]");
        output.hidden = false;
        output.innerHTML = `<p>${escape(errorMessage(error))}</p><details><summary>${t("details")}</summary><pre>${escape(error)}</pre></details>`;
        output.focus();
        return;
      } finally { saving = false; submitButton.disabled = false; }
      await close(true);
      try { await loadPage(); toast(t("saved")); }
      catch (error) { showError(error); toast(t("committed"), true); }
    });
  });
  document.body.append(dialog);
  dialog.showModal();
  return { dialog, close, markDirty };
}

async function confirmAction(message) {
  return new Promise((resolve) => {
    const opener = document.activeElement;
    const dialog = document.createElement("dialog");
    dialog.className = "app-dialog confirmation";
    dialog.setAttribute("aria-label", t("confirmAction"));
    dialog.innerHTML = `<h2>${t("confirmAction")}</h2><p>${escape(message)}</p><div class="modal-actions">${button(t("cancel"), "data-no")}${button(t("confirm"), "data-yes", "primary")}</div>`;
    const finish = (answer) => { dialog.close(); dialog.remove(); opener?.focus(); resolve(answer); };
    dialog.querySelector("[data-no]").onclick = () => finish(false);
    dialog.querySelector("[data-yes]").onclick = () => finish(true);
    dialog.oncancel = (event) => { event.preventDefault(); finish(false); };
    document.body.append(dialog);
    dialog.showModal();
  });
}

function shell() {
  document.documentElement.lang = language;
  app.innerHTML = `<div class="shell"><aside class="sidebar">
    <div class="brand"><div class="brand-icon" aria-hidden="true">B</div><strong>${t("appTitle")}</strong></div>
    <nav aria-label="${t("appTitle")}">${["books", "patrons", "loans", "settings"].map((name) =>
      button(t(name), `data-page="${name}" ${page === name ? 'aria-current="page"' : ""}`, `nav-item ${page === name ? "selected" : ""}`)).join("")}</nav>
    <p class="sidebar-note">${escape(info?.libraryPath || t("welcome"))}</p></aside>
    <main class="main"><div id="global-error" class="error-panel" role="alert" hidden></div><div id="page"></div></main>
    <div id="toast" class="toast" role="status" aria-live="polite"></div></div>`;
  app.querySelectorAll("[data-page]").forEach((node) => node.addEventListener("click", () => act(async () => {
    if (hasUnsaved() && !await confirmAction(t("discard"))) return;
    settingsDirty = false;
    page = node.dataset.page;
    Object.keys(query).forEach((key) => delete query[key]);
    Object.assign(query, { search: "", page: 1, pageSize: 50, sort: page === "loans" ? "dueDate" : page === "patrons" ? "name" : "title", descending: false });
    await loadPage();
  })));
}

function filters() {
  const sortOptions = page === "books" ? ["title", "author", "publicationYear"]
    : page === "patrons" ? ["name", "patronIdentifier"] : ["dueDate"];
  return `<form id="filters" class="filters">
    <div>${field("search", query.search)}</div>
    <div>${select("sort", sortOptions.map((name) => [name, t(name === "dueDate" ? "due_date" : name)]), query.sort)}</div>
    <label class="check"><input type="checkbox" name="descending" ${query.descending ? "checked" : ""}>${t("descending")}</label>
    ${page === "books" ? `<label class="check"><input type="checkbox" name="availableOnly" ${query.availableOnly ? "checked" : ""}>${t("availableOnly")}</label>` : ""}
    ${page === "loans" ? `<div>${select("status", [["", t("all")], ["active", t("active")], ["completed", t("completed")]], query.status || "")}</div>
      <label class="check"><input type="checkbox" name="overdueOnly" ${query.overdueOnly ? "checked" : ""}>${t("overdue")}</label>
      <div><button type="button" class="button secondary" id="patron-filter">${t("selectPatron")}</button><span id="patron-filter-name">${escape(query.patronLabel || "")}</span>
      ${query.patronId ? button(t("clearFilter"), 'id="clear-patron"') : ""}</div>` : ""}
    <button class="button primary" type="submit">${t("search")}</button>
  </form>`;
}

function pagination(result) {
  const totalPages = Math.max(1, Math.ceil(result.total / query.pageSize));
  return `<div class="pagination" role="navigation" aria-label="${t("page")}">
    ${button(t("previous"), `data-page-step="-1" ${query.page <= 1 ? "disabled" : ""}`)}
    <span role="status">${t("page")} ${query.page} / ${totalPages} · ${t("total")}: ${result.total}</span>
    ${button(t("next"), `data-page-step="1" ${query.page >= totalPages ? "disabled" : ""}`)}
    <div>${select("pageSize", [25, 50, 100].map((size) => [size, size]), query.pageSize)}</div></div>`;
}

function wireList() {
  document.querySelector("#filters")?.addEventListener("submit", (event) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    Object.assign(query, { search: form.get("search"), sort: form.get("sort"), descending: form.has("descending"),
      availableOnly: form.has("availableOnly"), overdueOnly: form.has("overdueOnly"), status: form.get("status") || null, page: 1 });
    void act(loadPage);
  });
  document.querySelectorAll("[data-page-step]").forEach((node) => node.onclick = () => act(async () => { query.page += Number(node.dataset.pageStep); await loadPage(); }));
  document.querySelector('select[name="pageSize"]')?.addEventListener("change", (event) => {
    query.pageSize = Number(event.target.value); query.page = 1; void act(loadPage);
  });
  document.querySelector("#clear-patron")?.addEventListener("click", () => act(async () => {
    delete query.patronId; delete query.patronLabel; query.page = 1; await loadPage();
  }));
  document.querySelector("#patron-filter")?.addEventListener("click", () => act(async () => {
    await choose("patrons", {}, async (patron) => {
      query.patronId = patron.id; query.patronLabel = patron.name; query.page = 1; await loadPage();
    });
  }));
}

async function loadPage() {
  const sequence = ++requestNumber;
  info = await invoke("app_info");
  if (sequence !== requestNumber) return;
  language = info.settings.language;
  shell();
  await getCurrentWindow().setTitle(t("appTitle"));
  if (!info.libraryOpen && page !== "settings") return welcome();
  if (page === "settings") return settings();
  const target = document.querySelector("#page");
  target.innerHTML = `<p role="status">${t("loading")}</p>`;
  const result = await invoke(`query_${page}`, { query: { ...query, patronLabel: undefined } });
  if (sequence !== requestNumber) return;
  const lastPage = Math.max(1, Math.ceil(result.total / query.pageSize));
  if (query.page > lastPage) { query.page = lastPage; return loadPage(); }
  const actions = page === "books" ? `${button(csvText(language, "import"), 'id="import-books"')}${button(t("addBook"), 'id="add-book"', "primary")}`
    : page === "patrons" ? button(t("addPatron"), 'id="add-patron"', "primary") : button(t("checkout"), 'id="checkout"', "primary");
  target.innerHTML = `<header class="page-header"><h1>${t(page)}</h1><div class="header-actions">${actions}</div></header>
    ${filters()}${page === "books" ? booksView(result.items) : page === "patrons" ? patronsView(result.items) : loansView(result.items)}
    ${pagination(result)}`;
  wireList();
  document.querySelector("#add-book")?.addEventListener("click", () => act(() => bookForm()));
  document.querySelector("#add-patron")?.addEventListener("click", () => act(() => patronForm()));
  document.querySelector("#checkout")?.addEventListener("click", () => act(checkoutForm));
  document.querySelector("#import-books")?.addEventListener("click", () => act(() => importBooks({
    language, notify: toast,
    onStateChange: (state) => { csvState = state; },
    onImported: async (count) => { query.search = ""; query.page = 1; await loadPage(); toast(csvText(language, "imported", { count })); },
  })));
  document.querySelectorAll("[data-book]").forEach((node) => node.onclick = () => act(() => bookDetail(result.items.find((book) => book.id === node.dataset.book))));
  document.querySelectorAll("[data-patron]").forEach((node) => node.onclick = () => act(() => patronForm(result.items.find((patron) => patron.id === node.dataset.patron))));
  document.querySelectorAll("[data-deactivate]").forEach((node) => node.onclick = () => act(async () => {
    if (await confirmAction(t("deactivate"))) { await invoke("deactivate_patron", { id: node.dataset.deactivate }); await loadPage(); }
  }));
  document.querySelectorAll("[data-return]").forEach((node) => node.onclick = () => act(async () => {
    if (await confirmAction(t("returnLoan"))) { await invoke("return_loan", { loanId: node.dataset.return }); await loadPage(); toast(t("committed")); }
  }));
  document.querySelectorAll("[data-resolve]").forEach((node) => node.onclick = () => act(async () => {
    await modal(t("resolveLost"), textarea("resolutionNote", "", "required"), async (values) => {
      if (!await confirmAction(t("resolveLost"))) throw new Error(t("cancel"));
      await invoke("resolve_lost_loan", { loanId: node.dataset.resolve, note: values.resolutionNote });
    });
  }));
  lazyCovers();
}

function booksView(items) {
  if (!items.length) return `<p class="empty-state">${t("empty")}</p>`;
  return `<section class="book-grid" aria-label="${t("books")}">${items.map((book) => `<article class="book-card">
    <button type="button" class="cover-button" data-book="${escape(book.id)}" aria-label="${escape(book.title)}">
      ${book.coverImage ? `<img loading="lazy" class="book-cover" data-cover="${escape(book.id)}" alt="${escape(book.title)}">` : `<span class="cover-placeholder">${t("noCover")}</span>`}</button>
    <div class="book-copy"><h2>${escape(book.title)}</h2><p>${escape(book.authors.join("; "))}</p><p>${escape(book.ddc)} ${escape(book.lcc)}</p>
    <p>${t("copies")}: ${book.totalCopies} · ${t("available")}: ${book.availableCopies}</p>${button(t("edit"), `data-book="${escape(book.id)}"`)}</div></article>`).join("")}</section>`;
}

function patronsView(items) {
  return `<div class="table-wrap"><table><caption>${t("patrons")}</caption><thead><tr>${["name", "patronIdentifier", "contactInformation", "status"].map((key) => `<th scope="col">${t(key)}</th>`).join("")}<th scope="col">${t("edit")}</th></tr></thead>
    <tbody>${items.map((patron) => `<tr><td>${escape(patron.name)}</td><td>${escape(patron.patronIdentifier)}</td><td>${escape(patron.contactInformation)}</td><td>${t(patron.active ? "active" : "inactive")}</td><td>
    ${button(t("edit"), `data-patron="${escape(patron.id)}"`)}${patron.active ? button(t("deactivate"), `data-deactivate="${escape(patron.id)}"`) : ""}</td></tr>`).join("")}</tbody></table>${!items.length ? `<p>${t("empty")}</p>` : ""}</div>`;
}

function loansView(items) {
  return `<div class="table-wrap"><table><caption>${t("loans")}</caption><thead><tr>${["books", "patrons", "copyIdentifier", "checkoutDate", "due_date", "returnDate", "closedAt", "status", "resolutionNote"].map((key) => `<th scope="col">${t(key)}</th>`).join("")}<th scope="col">${t("edit")}</th></tr></thead>
    <tbody>${items.map((item) => `<tr><td>${escape(item.bookTitle)}</td><td>${escape(item.patronName)}</td><td>${escape(item.copyIdentifier)}</td><td>${date(item.loan.checkoutDate)}</td><td>${date(item.loan.dueDate)}</td>
    <td>${date(item.loan.returnDate)}</td><td>${date(item.loan.closedAt)}</td><td>${item.loan.closureReason ? t(item.loan.closureReason) : `${t("active")}${item.copyStatus === "lost" ? ` · ${t("lost")}` : ""}${item.overdue ? ` · ${t("overdue")}` : ""}`}</td><td>${escape(item.loan.resolutionNote)}</td><td>
    ${!item.loan.closedAt ? button(t("returnLoan"), `data-return="${escape(item.loan.id)}"`) : ""}
    ${!item.loan.closedAt && item.copyStatus === "lost" ? button(t("resolveLost"), `data-resolve="${escape(item.loan.id)}"`) : ""}</td></tr>`).join("")}</tbody></table>${!items.length ? `<p>${t("empty")}</p>` : ""}</div>`;
}

function lazyCovers() {
  coverObserver?.disconnect();
  coverObserver = new IntersectionObserver((entries) => {
    entries.filter((entry) => entry.isIntersecting).forEach(({ target }) => {
      coverObserver.unobserve(target);
      void loadCover(target);
    });
  }, { rootMargin: "200px" });
  document.querySelectorAll("[data-cover]").forEach((image) => coverObserver.observe(image));
}

async function loadCover(image) {
  const failed = () => {
    const message = document.createElement("span");
    message.className = "cover-placeholder cover-error";
    message.textContent = t("coverError");
    image.replaceWith(message);
  };
  try {
    const src = await invoke("cover_data", { bookId: image.dataset.cover });
    if (!src) { failed(); return; }
    image.onerror = failed;
    image.src = src;
  } catch { failed(); }
}

async function bookForm(book = {}) {
  return modal(t(book.id ? "edit" : "addBook"), [
    field("title", book.title, "required"), field("authors", book.authors?.join("; ")),
    field("isbn", book.isbn), field("publisher", book.publisher),
    field("publicationYear", book.publicationYear, 'type="number" min="0" max="2147483647" step="1"'),
    field("genre", book.genre), `<p id="classification-help">${t("classificationHint")}</p>`,
    field("ddc", book.ddc, 'aria-describedby="classification-help"'), field("lcc", book.lcc, 'aria-describedby="classification-help"'), textarea("notes", book.notes),
  ].join(""), async (values) => invoke("save_book", {
    ...values, id: book.id || null, authors: values.authors.split(";").map((author) => author.trim()).filter(Boolean),
    publicationYear: values.publicationYear ? Number(values.publicationYear) : null,
  }));
}

async function bookDetail(book) {
  const { dialog, close } = await modal(book.title, `<div class="book-detail">
    <div class="detail-cover">${book.coverImage ? `<img data-cover="${escape(book.id)}" alt="${escape(book.title)}">` : `<span class="cover-placeholder">${t("noCover")}</span>`}</div>
    <div><p>${escape(book.authors.join("; "))}</p><dl>${["isbn", "publisher", "publicationYear", "genre", "ddc", "lcc", "notes"].map((key) => `<dt>${key === "isbn" ? "ISBN" : t(key)}</dt><dd>${escape(book[key] ?? "—")}</dd>`).join("")}</dl>
    <p>${t("coverHint")}</p><div class="header-actions">${button(t("edit"), "data-edit")}${button(t("changeCover"), "data-cover-pick")}${book.coverImage ? button(t("removeCover"), "data-cover-remove") : ""}${button(t("deleteBook"), "data-delete")}</div></div></div>
    <h3>${t("copies")}</h3><div id="copy-results"></div>${button(t("addCopy"), "data-add-copy")}`);
  dialog.querySelector("[data-edit]").onclick = () => act(async () => { await close(true); await bookForm(book); });
  dialog.querySelector("[data-cover-pick]").onclick = () => act(async () => {
    const path = await open({ multiple: false, filters: [{ name: "JPEG / PNG", extensions: ["jpg", "jpeg", "png"] }] });
    if (!path) return;
    await runOperation((operationId) => invoke("set_cover", { bookId: book.id, sourcePath: path, operationId }));
    await close(true); await loadPage();
  });
  dialog.querySelector("[data-cover-remove]")?.addEventListener("click", () => act(async () => {
    if (!await confirmAction(t("removeCover"))) return;
    await invoke("remove_cover", { bookId: book.id }); await close(true); await loadPage();
  }));
  dialog.querySelector("[data-delete]").onclick = () => act(async () => {
    if (!await confirmAction(t("deleteBook"))) return;
    await invoke("delete_book", { id: book.id }); await close(true); await loadPage();
  });
  dialog.querySelector("[data-add-copy]").onclick = () => act(async () => { await close(true); await copyForm({ bookId: book.id }); });
  const image = dialog.querySelector("[data-cover]");
  if (image) void loadCover(image);
  await copyList(dialog.querySelector("#copy-results"), book.id, () => close(true));
}

async function copyList(target, bookId, closeParent, local = { search: "", page: 1, pageSize: 50, sort: "location" }) {
  const result = await invoke("query_copies", { query: { ...local, bookId } });
  target.innerHTML = `<form class="copy-filters"><div>${field("search", local.search)}</div>
    <div>${field("location", local.location)}</div>
    <div>${select("status", [["", t("all")], ...["available", "checked_out", "lost", "withdrawn"].map((key) => [key, t(key)])], local.status || "")}</div>
    <div>${select("sort", [["location", t("location")], ["copyIdentifier", t("copyIdentifier")]], local.sort)}</div>
    <div>${select("pageSize", [25, 50, 100].map((size) => [size, size]), local.pageSize)}</div>
    <label class="check"><input type="checkbox" name="descending" ${local.descending ? "checked" : ""}>${t("descending")}</label>
    <button class="button primary">${t("search")}</button></form>
    <div class="copy-list">${result.items.map((copy) => `<div class="copy-row"><span>${escape(copy.copyIdentifier)} · ${t(copy.status)} · ${escape(copy.location)}</span>
      ${button(t("edit"), `data-copy-edit="${escape(copy.id)}"`)}
      ${["available", "checked_out"].includes(copy.status) ? button(t("markLost"), `data-status="lost" data-id="${escape(copy.id)}"`) : ""}
      ${copy.status === "lost" ? button(t("recover"), `data-status="available" data-id="${escape(copy.id)}"`) : ""}
      ${["available", "lost"].includes(copy.status) ? button(t("withdraw"), `data-status="withdrawn" data-id="${escape(copy.id)}"`) : ""}
      ${button(t("deleteCopy"), `data-copy-delete="${escape(copy.id)}"`)}</div>`).join("")}</div>
    <div class="pagination">${button(t("previous"), `data-step="-1" ${local.page === 1 ? "disabled" : ""}`)}
      <span>${t("page")} ${local.page} · ${t("total")}: ${result.total}</span>
      ${button(t("next"), `data-step="1" ${local.page * local.pageSize >= result.total ? "disabled" : ""}`)}</div>`;
  target.querySelector(".copy-filters").onsubmit = (event) => {
    event.preventDefault(); const values = Object.fromEntries(new FormData(event.currentTarget));
    void act(() => copyList(target, bookId, closeParent, { ...local, ...values,
      pageSize: Number(values.pageSize), descending: values.descending === "on", page: 1 }));
  };
  target.querySelectorAll("[data-step]").forEach((node) => node.onclick = () => act(() => copyList(target, bookId, closeParent, { ...local, page: local.page + Number(node.dataset.step) })));
  target.querySelectorAll("[data-copy-edit]").forEach((node) => node.onclick = () => act(async () => {
    await closeParent(); await copyForm(result.items.find((copy) => copy.id === node.dataset.copyEdit));
  }));
  target.querySelectorAll("[data-status]").forEach((node) => node.onclick = () => act(async () => {
    if (!await confirmAction(node.textContent)) return;
    await invoke("update_copy_status", { id: node.dataset.id, status: node.dataset.status });
    await copyList(target, bookId, closeParent, local);
  }));
  target.querySelectorAll("[data-copy-delete]").forEach((node) => node.onclick = () => act(async () => {
    if (!await confirmAction(t("deleteCopy"))) return;
    await invoke("delete_copy", { id: node.dataset.copyDelete }); await copyList(target, bookId, closeParent, local);
  }));
}

async function copyForm(copy) {
  return modal(t(copy.id ? "edit" : "addCopy"), field("copyIdentifier", copy.copyIdentifier, "required")
    + field("location", copy.location) + field("acquisitionDate", copy.acquisitionDate, 'type="date"') + textarea("notes", copy.notes),
  async (values) => invoke(copy.id ? "update_copy" : "add_copy", {
    ...values, id: copy.id, bookId: copy.bookId, acquisitionDate: values.acquisitionDate || null,
  }));
}

async function patronForm(patron = {}) {
  return modal(t(patron.id ? "edit" : "addPatron"), field("name", patron.name, "required")
    + field("patronIdentifier", patron.patronIdentifier, "required") + field("contactInformation", patron.contactInformation)
    + textarea("notes", patron.notes), async (values) => invoke("save_patron", { ...values, id: patron.id || null }));
}

async function choose(collection, extra, onSelected) {
  const { dialog, close } = await modal(t(collection === "copies" ? "selectCopy" : "selectPatron"),
    `<div class="picker"><label>${t("search")}<input data-search></label>${button(t("search"), "data-search-button")}<div data-results></div></div>`);
  let pickerPage = 1;
  const render = async () => {
    const result = await invoke(`query_${collection}`, { query: { ...extra, search: dialog.querySelector("[data-search]").value, page: pickerPage, pageSize: 25 } });
    dialog.querySelector("[data-results]").innerHTML = result.items.map((item) => button(
      collection === "copies" ? `${item.copyIdentifier} · ${item.bookTitle || ""} · ${item.location || ""}` : `${item.name} · ${item.patronIdentifier}`,
      `data-select="${escape(item.id)}"`,
    )).join("") + `<div class="pagination">${button(t("previous"), `data-picker-step="-1" ${pickerPage === 1 ? "disabled" : ""}`)}<span>${t("page")} ${pickerPage} · ${t("total")}: ${result.total}</span>${button(t("next"), `data-picker-step="1" ${pickerPage * 25 >= result.total ? "disabled" : ""}`)}</div>`;
    dialog.querySelectorAll("[data-select]").forEach((node) => node.onclick = () => act(async () => {
      const item = result.items.find((item) => item.id === node.dataset.select);
      await close(true); await onSelected(item);
    }));
    dialog.querySelectorAll("[data-picker-step]").forEach((node) => node.onclick = () => act(async () => { pickerPage += Number(node.dataset.pickerStep); await render(); }));
  };
  dialog.querySelector("[data-search-button]").onclick = () => act(async () => { pickerPage = 1; await render(); });
  dialog.querySelector("[data-search]").onkeydown = (event) => {
    if (event.key === "Enter") { event.preventDefault(); pickerPage = 1; void act(render); }
  };
  await render();
}

async function checkoutForm() {
  let copy;
  let patron;
  const { dialog, markDirty } = await modal(t("checkout"),
    `${button(t("selectCopy"), "data-copy-picker")}<p data-copy-label></p>
    ${button(t("selectPatron"), "data-patron-picker")}<p data-patron-label></p>${field("dueDate", "", 'type="date"')}`,
    async (values) => {
      if (!copy || !patron) throw new Error(`${t("selectCopy")} / ${t("selectPatron")}`);
      await invoke("checkout", { copyId: copy.id, patronId: patron.id, dueDate: values.dueDate || null });
    });
  dialog.querySelector("[data-copy-picker]").onclick = () => act(() => choose("copies", { status: "available" }, (selected) => {
    copy = selected; dialog.querySelector("[data-copy-label]").textContent = `${selected.copyIdentifier} · ${selected.bookTitle || ""}`; markDirty();
  }));
  dialog.querySelector("[data-patron-picker]").onclick = () => act(() => choose("patrons", { status: "active" }, (selected) => {
    patron = selected; dialog.querySelector("[data-patron-label]").textContent = selected.name; markDirty();
  }));
}

async function chooseLibrary(create) {
  if (hasUnsaved() && !await confirmAction(t("discard"))) return;
  const path = await open({ directory: true, multiple: false, title: t("chooseDirectory") });
  if (!path) return;
  await invoke(create ? "create_library" : "open_library", { path });
  settingsDirty = false;
  await loadPage();
}

function libraryActions() {
  document.querySelectorAll("[data-create-library]").forEach((node) => node.onclick = () => act(() => chooseLibrary(true)));
  document.querySelectorAll("[data-open-library]").forEach((node) => node.onclick = () => act(() => chooseLibrary(false)));
  document.querySelectorAll("[data-restore]").forEach((node) => node.onclick = () => act(restore));
  document.querySelector("[data-retry-library]")?.addEventListener("click", () => act(async () => {
    if (info.libraryPath) await invoke("open_library", { path: info.libraryPath });
    await loadPage();
  }));
}

function welcome() {
  document.querySelector("#page").innerHTML = `<h1>${t("appTitle")}</h1><p>${t("welcome")}</p>
    ${info.startupError ? `<p role="alert">${t("missingLibrary")}</p><details><summary>${t("details")}</summary><pre>${escape(info.startupError)}</pre></details>${button(t("retry"), "data-retry-library")}` : ""}
    <p>${escape(info.defaultLibraryPath)}</p><div class="header-actions">${button(t("createLibrary"), "data-create-library", "primary")}${button(t("openLibrary"), "data-open-library")}${button(t("restore"), "data-restore")}</div>`;
  libraryActions();
}

function settings() {
  document.querySelector("#page").innerHTML = `<h1>${t("settings")}</h1><section class="settings-grid">
    <form id="settings-form" class="panel settings-panel">${select("language", [["en", "English"], ["es", "Español"], ["fr", "Français"]], language)}
      ${info.libraryOpen ? field("loanPeriodDays", info.settings.loanPeriodDays, 'type="number" min="1" max="3650" required') : ""}
      <button class="button primary">${t("save")}</button></form>
    <section class="panel settings-panel"><h2>${t("library")}</h2><p class="path-text">${escape(info.libraryPath || "")}</p>
    <div class="header-actions">${button(t("createLibrary"), "data-create-library")}${button(t("openLibrary"), "data-open-library")}${info.libraryOpen ? button(t("backup"), "data-backup") : ""}${button(t("restore"), "data-restore")}</div><p>${t("privacy")}</p></section></section>`;
  libraryActions();
  document.querySelector("#settings-form").oninput = () => { settingsDirty = true; };
  document.querySelector("#settings-form").onsubmit = (event) => {
    event.preventDefault(); const values = Object.fromEntries(new FormData(event.currentTarget));
    void act(async () => {
      await invoke("update_settings", { language: values.language, loanPeriodDays: Number(values.loanPeriodDays || 21) });
      settingsDirty = false;
      await loadPage(); toast(t("saved"));
    });
  };
  document.querySelector("[data-backup]")?.addEventListener("click", () => act(async () => {
    const path = await save({ defaultPath: "library-backup.zip", filters: [{ name: "ZIP", extensions: ["zip"] }] });
    if (!path || !await confirmAction(t("backup"))) return;
    await runOperation((operationId) => invoke("backup_library", { path, operationId }));
    toast(t("committed"));
  }));
}

async function restore() {
  if (hasUnsaved() && !await confirmAction(t("discard"))) return;
  const path = await open({ multiple: false, filters: [{ name: "ZIP", extensions: ["zip"] }] });
  if (!path) return;
  const destination = await open({ directory: true, multiple: false, title: t("chooseDirectory") });
  if (!destination) return;
  const restored = await runOperation((operationId) => invoke("restore_library", { path, destination, operationId }));
  if (await confirmAction(t("restoreComplete"))) {
    await invoke("open_library", { path: typeof restored === "string" ? restored : destination });
    settingsDirty = false;
    await loadPage();
  }
}

async function runOperation(work) {
  const operationId = crypto.randomUUID();
  const { dialog, close } = await modal(t("loading"), `<p role="status" aria-live="polite" data-progress>${t("loading")}</p><progress aria-label="${t("loading")}"></progress>`, undefined, { dismissible: false });
  operation = operationId;
  const cancelButtons = dialog.querySelectorAll("[data-close]");
  cancelButtons.forEach((node) => { node.disabled = true; });
  const cancel = button(t("cancel"), "data-cancel-operation");
  dialog.querySelector(".modal-actions").insertAdjacentHTML("beforeend", cancel);
  dialog.querySelector("[data-cancel-operation]").onclick = () => act(async () => {
    await invoke("cancel_operation", { operationId });
    dialog.querySelector("[data-progress]").textContent = t("cancelRequested");
  });
  dialog.addEventListener("cancel", (event) => {
    event.preventDefault();
    dialog.querySelector("[data-cancel-operation]").click();
  });
  const stopWatching = watchOperation(operationId, {
    update: ({ completed, total, cancellable }) => {
      const progress = dialog.querySelector("progress");
      if (total > 0) { progress.max = total; progress.value = completed; }
      else progress.removeAttribute("value");
      dialog.querySelector("[data-progress]").textContent = cancellable ? `${t("loading")} ${total > 0 ? `${completed} / ${total}` : ""}` : t("committing");
      dialog.querySelector("[data-cancel-operation]").disabled = !cancellable;
    },
    error: showError,
  });
  try { return await work(operationId); }
  finally { stopWatching(); operation = undefined; await close(true); }
}

window.addEventListener("beforeunload", (event) => {
  if (!closingWindow && (hasUnsaved() || operation || csvState.busy)) { event.preventDefault(); event.returnValue = ""; }
});

void act(async () => {
  const window = getCurrentWindow();
  await window.onCloseRequested(async (event) => {
    if (closingWindow || (!hasUnsaved() && !operation && !csvState.busy)) return;
    event.preventDefault();
    if (operation || csvState.busy) { toast(t("busyWait"), true); return; }
    if (await confirmAction(t("discard"))) {
      closingWindow = true;
      try { await window.close(); }
      catch (error) { closingWindow = false; showError(error); }
    }
  });
  info = await invoke("app_info");
  language = info.settings.language;
  shell();
  await loadPage();
});
