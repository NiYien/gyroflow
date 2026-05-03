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
      tbody.innerHTML = `<tr class="empty-row"><td colspan="11">无匹配项。</td></tr>`;
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
      ? `<span class="badge badge-cn">国内</span>`
      : `<span class="badge badge-global">海外</span>`;
    const statusBadge = downloaded
      ? `<span class="badge badge-downloaded">已下载</span>`
      : `<span class="badge badge-pending">待下载</span>`;

    const actions = [];
    if (busy) {
      actions.push(`<span class="spinner"></span>处理中...`);
    } else {
      if (downloaded) {
        actions.push(`<button class="btn btn-sm" data-act="open">打开本地</button>`);
        actions.push(`<button class="btn btn-sm" data-act="copy_prompt">复制 prompt</button>`);
        actions.push(`<button class="btn btn-sm" data-act="redownload">重新下载</button>`);
      } else {
        actions.push(`<button class="btn btn-sm" data-act="download">下载</button>`);
      }
      actions.push(`<button class="btn btn-sm btn-danger" data-act="delete">删除</button>`);
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
        <textarea class="notes-editor" rows="1" placeholder="本地备注（不上传）">${escape(row.notes || "")}</textarea>
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
          toast("备注保存失败：" + e.message, "error");
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
    el.textContent = state.rows.length ? `共 ${state.rows.length} 行` : "";
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
    btn.textContent = "刷新中…";
    try {
      const res = await api().refresh(since, limit);
      if (!res.ok) throw new Error(res.error);
      toast(`刷新完成：新增 ${res.data.inserted} 条，更新 ${res.data.updated} 条`, "success");
      await applyFilter();
    } catch (e) {
      toast("刷新失败：" + e.message, "error");
    } finally {
      btn.disabled = false;
      btn.textContent = "刷新";
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
      toast("列表加载失败：" + e.message, "error");
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
        const ok = window.confirm(`${row.id} 已下载。是否重新下载并覆盖？`);
        if (!ok) return;
      }
      setRowBusy(row.id, true);
      try {
        const res = await api().download_one(row.id, act === "redownload");
        if (!res.ok) throw new Error(res.error);
        toast(`已下载 ${row.id}`, "success");
      } catch (e) {
        toast("下载失败：" + e.message, "error");
      } finally {
        setRowBusy(row.id, false);
        await applyFilter();
      }
    } else if (act === "open") {
      try {
        const res = await api().open_local(row.id);
        if (!res.ok) throw new Error(res.error);
      } catch (e) {
        toast("打开失败：" + e.message, "error");
      }
    } else if (act === "copy_prompt") {
      try {
        const res = await api().copy_prompt(row.id);
        if (!res.ok) throw new Error(res.error);
        const mech = res.data.mechanism;
        if (mech.startsWith("file:")) {
          toast(`剪贴板工具不可用；已保存到 ${mech.slice(5)}`, "warn", 6000);
        } else {
          toast(`Prompt 已复制（${res.data.chars} 字符），粘贴到 Claude 即可。`, "success");
        }
      } catch (e) {
        toast("复制 prompt 失败：" + e.message, "error");
      }
    } else if (act === "delete") {
      const ok = window.confirm(
        `确认删除反馈 ${row.id}？\n描述：${row.summary || "（无）"}\n\n` +
        `将同时从 R2/123、KV 索引、本地缓存移除。`
      );
      if (!ok) return;
      setRowBusy(row.id, true);
      try {
        const res = await api().delete_one(row.id);
        if (!res.ok) throw new Error(res.error);
        const failures = (res.data && res.data.failures) || [];
        if (failures.length) {
          toast("删除完成（含警告）：" + failures.join("; "), "warn", 6000);
        } else {
          toast(`已删除 ${row.id}`, "success");
        }
      } catch (e) {
        toast("删除失败：" + e.message, "error");
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
      `确认删除 ${ids.length} 条反馈？\n\n  ${head}${tail}\n\n` +
      `将逐条从 R2/123 + KV + 本地缓存移除（顺序执行，非并行）。`
    );
    if (!ok) return;
    const btn = document.getElementById("batch-delete-btn");
    btn.disabled = true;
    btn.textContent = "删除中…";
    try {
      const res = await api().delete_many(ids);
      if (!res.ok) throw new Error(res.error);
      const succ = res.data.succeeded;
      const fail = res.data.failed.length;
      const cls = fail ? "warn" : "success";
      toast(`批量删除完成：成功 ${succ}，失败 ${fail}`, cls, 6000);
      if (fail) {
        console.warn("Failed deletes:", res.data.failed);
      }
      state.selected.clear();
    } catch (e) {
      toast("批量删除失败：" + e.message, "error");
    } finally {
      btn.textContent = `批量删除 (0)`;
      await applyFilter();
    }
  }

  async function onClean() {
    const days = parseInt(document.getElementById("clean-days").value, 10);
    if (isNaN(days) || days < 0) {
      toast("无效的天数阈值", "error");
      return;
    }
    const ok = window.confirm(`确认清理所有早于 ${days} 天的已下载解压目录？`);
    if (!ok) return;
    try {
      const res = await api().clean_cache(days);
      if (!res.ok) throw new Error(res.error);
      toast(`已清理 ${res.data.cleaned} 个解压目录`, "success");
      await applyFilter();
    } catch (e) {
      toast("清理失败：" + e.message, "error");
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
          ping.data.niyien_api_base + " — 缓存 @ " + ping.data.cache_root;
      }
    } catch (e) {
      toast("初始化连接失败：" + e.message, "error");
    }
    await applyFilter();
    await refreshCacheSize();
  });
})();
