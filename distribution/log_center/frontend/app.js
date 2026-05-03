/* NiYien Feedback Log Center — frontend logic.
 *
 * Bridges to the Python BackendAPI via window.pywebview.api.<method>.
 * Every backend call returns {ok, data, error}; we surface errors as
 * toasts and keep the table state in a single `state` object.
 */

(() => {
  "use strict";

  const state = {
    rows: [],            // last fetched rows (raw from backend)
    selected: new Set(), // row ids currently checked
    busy: new Set(),     // row ids currently performing an action
    notesTimers: {},     // debounce timers for inline notes
  };

  // ---------------- pywebview bridge readiness ----------------

  function whenApiReady(fn) {
    if (window.pywebview && window.pywebview.api) {
      fn(window.pywebview.api);
      return;
    }
    window.addEventListener("pywebviewready", () => fn(window.pywebview.api), { once: true });
  }

  function api() {
    if (!window.pywebview || !window.pywebview.api) {
      throw new Error("pywebview bridge not ready");
    }
    return window.pywebview.api;
  }

  // ---------------- toast ----------------

  function toast(message, type = "info", ttlMs = 3500) {
    const container = document.getElementById("toast-container");
    const el = document.createElement("div");
    el.className = "toast toast-" + type;
    el.textContent = message;
    container.appendChild(el);
    setTimeout(() => el.remove(), ttlMs);
  }

  // ---------------- helpers ----------------

  function escape(text) {
    if (text == null) return "";
    return String(text)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function truncate(text, n) {
    if (text == null) return "";
    const s = String(text);
    return s.length > n ? s.slice(0, n - 1) + "…" : s;
  }

  function formatTimestamp(iso) {
    if (!iso) return "";
    try {
      const d = new Date(iso);
      if (isNaN(d.getTime())) return iso;
      const pad = (n) => String(n).padStart(2, "0");
      return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
    } catch (e) {
      return iso;
    }
  }

  function isoFromInput(value) {
    if (!value) return null;
    return new Date(value + "T00:00:00Z").toISOString();
  }

  // ---------------- rendering ----------------

  function renderTable() {
    const tbody = document.getElementById("feedback-tbody");
    tbody.innerHTML = "";
    if (!state.rows.length) {
      tbody.innerHTML = `<tr class="empty-row"><td colspan="11">No matches.</td></tr>`;
      updateRowCount();
      return;
    }
    for (const row of state.rows) {
      tbody.appendChild(renderRow(row));
    }
    updateRowCount();
    updateBatchButton();
  }

  function renderRow(row) {
    const tr = document.createElement("tr");
    tr.dataset.id = row.id;

    const checked = state.selected.has(row.id) ? "checked" : "";
    const busy = state.busy.has(row.id);
    const downloaded = !!row.downloaded;

    const regionBadge = row.region === "cn"
      ? `<span class="badge badge-cn">cn</span>`
      : `<span class="badge badge-global">global</span>`;
    const statusBadge = downloaded
      ? `<span class="badge badge-downloaded">Downloaded</span>`
      : `<span class="badge badge-pending">Pending</span>`;

    const actions = [];
    if (busy) {
      actions.push(`<span class="spinner"></span>working...`);
    } else {
      if (downloaded) {
        actions.push(`<button class="btn btn-sm" data-act="open">Open local</button>`);
        actions.push(`<button class="btn btn-sm" data-act="copy_prompt">Copy prompt</button>`);
        actions.push(`<button class="btn btn-sm" data-act="redownload">Re-download</button>`);
      } else {
        actions.push(`<button class="btn btn-sm" data-act="download">Download</button>`);
      }
      actions.push(`<button class="btn btn-sm btn-danger" data-act="delete">Delete</button>`);
    }

    tr.innerHTML = `
      <td class="col-check"><input type="checkbox" class="row-check" ${checked}></td>
      <td class="col-id">${escape(row.id)}</td>
      <td class="col-region">${regionBadge}</td>
      <td class="col-ts">${escape(formatTimestamp(row.ts))}</td>
      <td class="col-version">${escape(row.app_version || "")}</td>
      <td class="col-env">${escape((row.os || "") + (row.gpu ? " / " + row.gpu : ""))}</td>
      <td class="col-summary">
        <span class="summary-cell" title="${escape(row.summary || "")}">${escape(truncate(row.summary, 80))}</span>
        <textarea class="notes-editor" rows="1" placeholder="local notes (not uploaded)">${escape(row.notes || "")}</textarea>
      </td>
      <td class="col-email">${escape(row.email || "")}</td>
      <td class="col-size">${escape(row.size_human || "")}</td>
      <td class="col-status">${statusBadge}</td>
      <td class="col-actions"><div class="row-actions">${actions.join("")}</div></td>
    `;

    // Wire up checkbox.
    tr.querySelector(".row-check").addEventListener("change", (e) => {
      if (e.target.checked) state.selected.add(row.id);
      else state.selected.delete(row.id);
      updateBatchButton();
    });

    // Wire up notes (debounced auto-save).
    const notesEl = tr.querySelector(".notes-editor");
    notesEl.addEventListener("input", () => {
      clearTimeout(state.notesTimers[row.id]);
      state.notesTimers[row.id] = setTimeout(async () => {
        try {
          await api().update_notes(row.id, notesEl.value);
          row.notes = notesEl.value;
        } catch (e) {
          toast("notes save failed: " + e.message, "error");
        }
      }, 600);
    });

    // Wire up action buttons.
    tr.querySelectorAll("button[data-act]").forEach((btn) => {
      btn.addEventListener("click", () => onRowAction(row, btn.dataset.act));
    });

    return tr;
  }

  function updateRowCount() {
    const el = document.getElementById("row-count");
    el.textContent = state.rows.length ? `${state.rows.length} rows` : "";
  }

  function updateBatchButton() {
    const btn = document.getElementById("batch-delete-btn");
    const cnt = document.getElementById("selected-count");
    cnt.textContent = String(state.selected.size);
    btn.disabled = state.selected.size === 0;
    const all = document.getElementById("select-all");
    all.checked = state.rows.length > 0 && state.rows.every((r) => state.selected.has(r.id));
  }

  function setRowBusy(id, busy) {
    if (busy) state.busy.add(id);
    else state.busy.delete(id);
    renderTable();
  }

  // ---------------- actions ----------------

  async function refresh() {
    const since = isoFromInput(document.getElementById("filter-since").value);
    const limit = parseInt(document.getElementById("filter-limit").value, 10) || 500;
    const btn = document.getElementById("refresh-btn");
    btn.disabled = true;
    btn.textContent = "Refreshing…";
    try {
      const res = await api().refresh(since, limit);
      if (!res.ok) throw new Error(res.error);
      toast(`Refreshed: +${res.data.inserted} new, ${res.data.updated} updated`, "success");
      await applyFilter();
    } catch (e) {
      toast("Refresh failed: " + e.message, "error");
    } finally {
      btn.disabled = false;
      btn.textContent = "Refresh";
    }
  }

  async function applyFilter() {
    const filters = {
      since: isoFromInput(document.getElementById("filter-since").value),
      until: isoFromInput(document.getElementById("filter-until").value),
      region: document.getElementById("filter-region").value,
      downloaded: document.getElementById("filter-downloaded").value || null,
      limit: parseInt(document.getElementById("filter-limit").value, 10) || 500,
    };
    try {
      const res = await api().list(filters);
      if (!res.ok) throw new Error(res.error);
      state.rows = res.data;
      // Drop selections that no longer match filter.
      const visible = new Set(state.rows.map((r) => r.id));
      for (const id of Array.from(state.selected)) {
        if (!visible.has(id)) state.selected.delete(id);
      }
      renderTable();
      await refreshCacheSize();
    } catch (e) {
      toast("List failed: " + e.message, "error");
    }
  }

  async function refreshCacheSize() {
    try {
      const res = await api().get_cache_size();
      if (res.ok) {
        document.getElementById("cache-size").textContent = res.data.human;
      }
    } catch (e) { /* non-fatal */ }
  }

  async function onRowAction(row, act) {
    if (act === "download" || act === "redownload") {
      if (act === "redownload") {
        const ok = window.confirm(`Already downloaded (${row.id}). Re-download and overwrite?`);
        if (!ok) return;
      }
      setRowBusy(row.id, true);
      try {
        const res = await api().download_one(row.id, act === "redownload");
        if (!res.ok) throw new Error(res.error);
        toast(`Downloaded ${row.id}`, "success");
      } catch (e) {
        toast("Download failed: " + e.message, "error");
      } finally {
        setRowBusy(row.id, false);
        await applyFilter();
      }
    } else if (act === "open") {
      try {
        const res = await api().open_local(row.id);
        if (!res.ok) throw new Error(res.error);
      } catch (e) {
        toast("Open failed: " + e.message, "error");
      }
    } else if (act === "copy_prompt") {
      try {
        const res = await api().copy_prompt(row.id);
        if (!res.ok) throw new Error(res.error);
        const mech = res.data.mechanism;
        if (mech.startsWith("file:")) {
          toast(`Clipboard tool unavailable; saved to ${mech.slice(5)}`, "warn", 6000);
        } else {
          toast(`Prompt copied (${res.data.chars} chars). Paste into Claude.`, "success");
        }
      } catch (e) {
        toast("Copy prompt failed: " + e.message, "error");
      }
    } else if (act === "delete") {
      const ok = window.confirm(
        `Delete feedback ${row.id}?\nSummary: ${row.summary || "(none)"}\n\n` +
        `This removes the file from R2/123, the KV index entry, and the local cache.`
      );
      if (!ok) return;
      setRowBusy(row.id, true);
      try {
        const res = await api().delete_one(row.id);
        if (!res.ok) throw new Error(res.error);
        const failures = (res.data && res.data.failures) || [];
        if (failures.length) {
          toast("Deleted with warnings: " + failures.join("; "), "warn", 6000);
        } else {
          toast(`Deleted ${row.id}`, "success");
        }
      } catch (e) {
        toast("Delete failed: " + e.message, "error");
      } finally {
        setRowBusy(row.id, false);
        await applyFilter();
      }
    }
  }

  async function onBatchDelete() {
    if (state.selected.size === 0) return;
    const ids = Array.from(state.selected);
    const head = ids.slice(0, 10).join("\n  ");
    const tail = ids.length > 10 ? `\n  ... (${ids.length - 10} more)` : "";
    const ok = window.confirm(
      `Delete ${ids.length} feedback record(s)?\n\n  ${head}${tail}\n\n` +
      `Each one will be removed from R2/123 + KV + local cache. Sequential, not parallel.`
    );
    if (!ok) return;
    const btn = document.getElementById("batch-delete-btn");
    btn.disabled = true;
    btn.textContent = "Deleting…";
    try {
      const res = await api().delete_many(ids);
      if (!res.ok) throw new Error(res.error);
      const succ = res.data.succeeded;
      const fail = res.data.failed.length;
      const cls = fail ? "warn" : "success";
      toast(`Batch delete done: ${succ} ok, ${fail} failed`, cls, 6000);
      if (fail) {
        console.warn("Failed deletes:", res.data.failed);
      }
      state.selected.clear();
    } catch (e) {
      toast("Batch delete failed: " + e.message, "error");
    } finally {
      btn.textContent = `Delete selected (0)`;
      await applyFilter();
    }
  }

  async function onClean() {
    const days = parseInt(document.getElementById("clean-days").value, 10);
    if (isNaN(days) || days < 0) {
      toast("Invalid threshold days", "error");
      return;
    }
    const ok = window.confirm(`Clean every downloaded extracted dir whose ts is older than ${days} days?`);
    if (!ok) return;
    try {
      const res = await api().clean_cache(days);
      if (!res.ok) throw new Error(res.error);
      toast(`Cleaned ${res.data.cleaned} extracted dir(s)`, "success");
      await applyFilter();
    } catch (e) {
      toast("Clean failed: " + e.message, "error");
    }
  }

  // ---------------- init ----------------

  function defaultSinceIso() {
    const d = new Date();
    d.setDate(d.getDate() - 30);
    return d.toISOString().slice(0, 10);
  }

  function setupHandlers() {
    document.getElementById("refresh-btn").addEventListener("click", refresh);
    document.getElementById("apply-filter-btn").addEventListener("click", applyFilter);
    document.getElementById("batch-delete-btn").addEventListener("click", onBatchDelete);
    document.getElementById("clean-btn").addEventListener("click", onClean);
    document.getElementById("select-all").addEventListener("change", (e) => {
      if (e.target.checked) state.rows.forEach((r) => state.selected.add(r.id));
      else state.selected.clear();
      renderTable();
    });
    document.getElementById("filter-since").value = defaultSinceIso();
  }

  whenApiReady(async (a) => {
    setupHandlers();
    try {
      const ping = await a.ping();
      if (ping.ok) {
        document.getElementById("connection-info").textContent =
          ping.data.niyien_api_base + " — cache @ " + ping.data.cache_root;
      }
    } catch (e) {
      toast("Init ping failed: " + e.message, "error");
    }
    await applyFilter();
    await refreshCacheSize();
  });
})();
