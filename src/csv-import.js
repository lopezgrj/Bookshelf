import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { csvText, csvMessages } from "./csv-locales.js";
import { translate } from "./locales.js";
import { watchOperation } from "./operation-progress.js";

const escape = (value) => String(value ?? "").replace(/[&<>"']/g, (char) => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
}[char]));

export async function importBooks({ language, onImported, notify, onStateChange = () => {} }) {
  const text = (key, values) => csvText(language, key, values);
  const opener = document.activeElement;
  const path = await open({ multiple: false, title: text("select"), filters: [{ name: "CSV", extensions: ["csv"] }] });
  if (!path) return;
  let preview;
  let busy = false;
  let operationId;
  let closed = false;
  let page = 1;
  const dialog = document.createElement("dialog");
  dialog.className = "csv-dialog";
  dialog.setAttribute("aria-labelledby", "csv-title");
  dialog.innerHTML = `<h2 id="csv-title">${text("preview")}</h2><p>${text("explanation")}</p>
    <label for="csv-delimiter">${text("delimiter")}</label><select id="csv-delimiter">
      <option value="">${text("auto")}</option><option value=",">${text("comma")}</option><option value="|">${text("pipe")}</option>
    </select><p data-progress role="status" aria-live="polite"></p><progress aria-label="${text("working")}" hidden></progress>
    <div data-preview></div><div data-error class="csv-errors" role="alert" hidden></div>
    <div class="modal-actions"><button class="button secondary" data-cancel>${text("cancel")}</button>
    <button class="button secondary" data-reselect>${text("chooseAgain")}</button>
    <button class="button primary" data-confirm disabled>${text("confirm")}</button></div>`;
  document.body.append(dialog);
  dialog.showModal();
  onStateChange({ open: true, busy: false });
  const confirm = dialog.querySelector("[data-confirm]");
  const cancelButton = dialog.querySelector("[data-cancel]");
  const delimiter = dialog.querySelector("#csv-delimiter");
  const report = (error) => {
    const code = String(error).split(":")[0];
    const message = Object.hasOwn(csvMessages[language], code) ? text(code) : text("error");
    const output = dialog.querySelector("[data-error]");
    output.hidden = false;
    output.innerHTML = `<p>${escape(message)}</p><details><summary>${text("details")}</summary><pre>${escape(error)}</pre></details>`;
  };
  const finish = () => {
    closed = true; dialog.close(); dialog.remove();
    onStateChange({ open: false, busy: false });
    if (opener?.isConnected) opener.focus();
  };
  const update = () => {
    onStateChange({ open: !closed, busy });
    confirm.disabled = busy || !preview || preview.errors.length > 0 || preview.rows.length === 0 || !preview.delimiter
      || (!!preview.unknownColumns.length && !dialog.querySelector("[data-ignore]")?.checked)
      || (!dialog.querySelector("[data-duplicates-label]")?.hidden && !dialog.querySelector("[data-duplicates]")?.checked);
    delimiter.disabled = busy;
    dialog.querySelector("[data-reselect]").disabled = busy;
  };
  const progress = () => {
    const bar = dialog.querySelector("progress");
    bar.hidden = false;
    bar.removeAttribute("value");
    cancelButton.disabled = false;
    const stop = watchOperation(operationId, {
      update: ({ completed, total, cancellable }) => {
        cancelButton.disabled = !cancellable;
        if (total > 0) { bar.max = total; bar.value = completed; }
        else bar.removeAttribute("value");
        dialog.querySelector("[data-progress]").textContent = cancellable
          ? `${text("working")} ${total > 0 ? `${completed} / ${total}` : ""}` : text("committing");
      },
      error: report,
    });
    return () => { stop(); bar.hidden = true; cancelButton.disabled = false; };
  };
  const cancel = async () => {
    try {
      if (busy) {
        if (cancelButton.disabled) return;
        await invoke("cancel_operation", { operationId });
        dialog.querySelector("[data-progress]").textContent = text("cancel_requested");
        return;
      }
      if (preview) await invoke("cancel_book_csv", { token: preview.token });
      finish();
    } catch (error) { report(error); }
  };
  dialog.addEventListener("cancel", (event) => { event.preventDefault(); void cancel(); });
  cancelButton.onclick = cancel;
  dialog.querySelector("[data-reselect]").onclick = async () => {
    await cancel();
    if (closed) await importBooks({ language, onImported, notify, onStateChange });
  };
  function render() {
    const rows = preview.rows.slice((page - 1) * 50, page * 50);
    const errors = preview.errors.slice((page - 1) * 50, page * 50);
    const pages = Math.max(1, Math.ceil(Math.max(preview.rows.length, preview.errors.length) / 50));
    dialog.querySelector("[data-preview]").innerHTML = `<p><strong>${text("count", { count: preview.rows.length })}</strong>
      · ${text("delimiter")}: ${escape(preview.delimiter || "—")}</p>
      ${preview.errors.length ? `<div class="csv-errors" role="alert"><p>${text("errors")} ${text("errorCount", { count: preview.errors.length })}</p><ul>${errors.map((error) => `
        <li>${escape(text("location", { record: error.record, column: error.column || "—" }))}: ${escape(csvMessages[language][error.code] || text("error"))}</li>`).join("")}</ul></div>` : ""}
      ${preview.unknownColumns.length ? `<p>${escape(text("unknown", { columns: preview.unknownColumns.join(", ") }))}</p>
        <label class="csv-check"><input type="checkbox" data-ignore>${text("ignore")}</label>` : ""}
      <div class="table-wrap csv-preview"><table><caption>${text("preview")}</caption><thead><tr>
        ${["record", "title", "authors", "year"].map((key) => `<th scope="col">${text(key)}</th>`).join("")}<th scope="col">ISBN</th>
        <th scope="col">DDC</th><th scope="col">LCC</th><th scope="col">${text("publisher")}</th><th scope="col">${text("genre")}</th><th scope="col">${text("notes")}</th><th scope="col">${text("duplicate")}</th>
      </tr></thead><tbody>${rows.map(({ record, book, duplicate }) => `<tr><td>${record}</td><td>${escape(book.title)}</td>
        <td>${escape(book.authors.join("; "))}</td><td>${escape(book.publicationYear)}</td><td>${escape(book.isbn)}</td>
        <td>${escape(book.ddc)}</td><td>${escape(book.lcc)}</td><td>${escape(book.publisher)}</td><td>${escape(book.genre)}</td>
        <td class="csv-notes">${escape(book.notes)}</td><td>${duplicate ? text("duplicate") : "—"}</td></tr>`).join("")}</tbody></table></div>
      <div class="pagination"><button type="button" class="button secondary" data-page-step="-1" ${page === 1 ? "disabled" : ""}>${text("previous")}</button>
      <span>${text("page")} ${page} / ${pages}</span>
      <button type="button" class="button secondary" data-page-step="1" ${page >= pages ? "disabled" : ""}>${text("next")}</button></div>
      ${!preview.rows.length ? `<p>${text("empty")}</p>` : ""}
      <label class="csv-check" data-duplicates-label ${preview.rows.some((row) => row.duplicate) ? "" : "hidden"}>
        <input type="checkbox" data-duplicates>${text("confirmDuplicates")}</label>`;
    dialog.querySelectorAll("[data-page-step]").forEach((button) => button.onclick = () => {
      const duplicates = dialog.querySelector("[data-duplicates]")?.checked;
      const ignore = dialog.querySelector("[data-ignore]")?.checked;
      page += Number(button.dataset.pageStep); render();
      dialog.querySelector("[data-duplicates]").checked = duplicates;
      if (dialog.querySelector("[data-ignore]")) dialog.querySelector("[data-ignore]").checked = ignore;
      update();
    });
    dialog.querySelectorAll("[data-preview] input").forEach((input) => input.onchange = update);
    update();
  }
  async function parse() {
    busy = true; operationId = crypto.randomUUID(); update();
    dialog.querySelector("[data-progress]").textContent = text("working");
    dialog.querySelector("[data-error]").hidden = true;
    const stop = progress();
    try {
      if (preview) await invoke("cancel_book_csv", { token: preview.token });
      preview = await invoke("preview_book_csv", { path, delimiter: delimiter.value || null, operationId });
      page = 1; render();
    } catch (error) {
      preview = undefined;
      dialog.querySelector("[data-preview]").innerHTML = "";
      report(error);
    } finally {
      stop(); busy = false; dialog.querySelector("[data-progress]").textContent = ""; update();
    }
  }
  delimiter.onchange = () => { void parse(); };
  confirm.onclick = async () => {
    busy = true; operationId = crypto.randomUUID(); update();
    dialog.querySelector("[data-progress]").textContent = text("working");
    const stop = progress();
    let count;
    try {
      count = await invoke("import_book_csv", {
        token: preview.token,
        allowDuplicates: dialog.querySelector("[data-duplicates]").checked,
        ignoreUnknownColumns: dialog.querySelector("[data-ignore]")?.checked ?? false,
        operationId,
      });
    } catch (error) {
      if (String(error).startsWith("CSV_DUPLICATES:")) dialog.querySelector("[data-duplicates-label]").hidden = false;
      report(error); busy = false; dialog.querySelector("[data-progress]").textContent = ""; update(); return;
    } finally {
      stop();
    }
    finish();
    try { await onImported(count); }
    catch (error) { notify(`${translate(language, "committed")} ${translate(language, "error")}`, true); }
  };
  await parse();
}
