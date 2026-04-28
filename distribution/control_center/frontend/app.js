// app.js — Dashboard + view switching

// ---- Utility ----

function logOutput(msg, data) {
  const out = document.getElementById('poc-output');
  if (!out) return;
  const ts = new Date().toLocaleTimeString();
  const payload = data === undefined ? '' : '\n' + JSON.stringify(data, null, 2);
  out.textContent = `[${ts}] ${msg}${payload}\n\n` + out.textContent;
}

function fmtRelativeTime(iso) {
  if (!iso) return '';
  const d = new Date(iso);
  if (isNaN(d)) return iso;
  const sec = Math.round((Date.now() - d.getTime()) / 1000);
  if (sec < 60) return `${sec} 秒前`;
  const min = Math.round(sec / 60);
  if (min < 60) return `${min} 分钟前`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr} 小时前`;
  const day = Math.round(hr / 24);
  if (day < 30) return `${day} 天前`;
  const mo = Math.round(day / 30);
  return `${mo} 个月前`;
}

function waitForApi() {
  return new Promise((resolve, reject) => {
    if (window.pywebview && pywebview.api) return resolve();
    let elapsed = 0;
    const interval = setInterval(() => {
      if (window.pywebview && pywebview.api) {
        clearInterval(interval);
        resolve();
      } else if ((elapsed += 100) > 5000) {
        clearInterval(interval);
        reject(new Error('pywebview bridge did not initialize within 5s'));
      }
    }, 100);
  });
}

// ---- Connection status ----

const connStatus = document.getElementById('conn-status');

function setConn(ok, text) {
  connStatus.textContent = text;
  connStatus.classList.remove('text-slate-500', 'text-emerald-600', 'text-red-600', 'text-amber-600');
  if (ok === true) connStatus.classList.add('text-emerald-600');
  else if (ok === false) connStatus.classList.add('text-red-600');
  else connStatus.classList.add('text-amber-600');
}

// ---- Dashboard state ----

function renderDashboard(state) {
  // App card
  const appEl = document.getElementById('app-version');
  const appMeta = document.getElementById('app-meta');
  if (state.app) {
    appEl.textContent = state.app.version || '-';
    const bits = [];
    if (state.app.tag) bits.push(state.app.tag);
    if (state.app.is_auto_pushed) bits.push('当前推送');
    if (state.app.recommended) bits.push('推荐');
    const base = bits.join(' · ') || ' ';
    if (state.app.missing_from_github) {
      appMeta.innerHTML = `${base} <span class="text-red-600 font-semibold">· ⚠ tag 已从 GitHub 删除</span>`;
    } else {
      appMeta.textContent = base;
    }
  } else {
    appEl.textContent = '-';
    if (state.errors.vercel) appMeta.textContent = 'Vercel 未连接';
    else if (state.errors.policy) appMeta.textContent = 'policy 加密态 · 待 decrypt';
    else appMeta.textContent = '无策略';
  }

  // Source tag helper
  function sourceTag(source) {
    if (source === 'vercel') return '<span class="text-emerald-600 text-[11px]">· vercel (已推送)</span>';
    if (source === 'defaults') return '<span class="text-amber-600 text-[11px]">· publish_defaults (未推送)</span>';
    return '<span class="text-slate-400 text-[11px]">· 未配置</span>';
  }

  // Lens
  const lensEl = document.getElementById('lens-version');
  const lensMeta = document.getElementById('lens-meta');
  if (state.lens && state.lens.tag) {
    lensEl.textContent = state.lens.tag;
    const bits = [];
    if (state.lens.version) bits.push(`v${state.lens.version}`);
    lensMeta.innerHTML = `${bits.join(' ')} ${sourceTag(state.lens.source)}`.trim();
  } else {
    lensEl.textContent = '-';
    lensMeta.innerHTML = state.errors.vercel ? 'Vercel 未连接' : sourceTag('none');
  }
  // Lens upgrade banner
  renderUpdateBanner(
    'lens-meta', 'lens_tag',
    (state.updates_available || {}).lens, '上游 lens 仓库',
  );

  // Plugin
  const pluginEl = document.getElementById('plugin-version');
  const pluginMeta = document.getElementById('plugin-meta');
  if (state.plugin && (state.plugin.tag || state.plugin.artifact_name)) {
    if (state.plugin.mode === 'artifact') {
      pluginEl.textContent = state.plugin.artifact_name ? '(artifact)' : '(最新 artifact)';
      const an = state.plugin.artifact_name || 'auto-latest';
      const short = an.length > 38 ? an.slice(0, 38) + '...' : an;
      pluginEl.title = state.plugin.artifact_name || '';
      pluginMeta.innerHTML = `${short} ${sourceTag(state.plugin.source)}`;
    } else {
      pluginEl.textContent = state.plugin.tag || '-';
      let pluginMetaHtml = `release mode ${sourceTag(state.plugin.source)}`;
      let pluginTitle = state.plugin.tag || '';
      if (state.plugin.missing_from_github) {
        pluginMetaHtml += ` <span class="text-red-600 font-semibold">· ⚠ tag 已从 GitHub 删除</span>`;
      }
      // pan123 probe (parallel to SDK card). Same red/amber/green ladder:
      //   non-empty missing_files = directory exists but missing some files
      //   pan123_error            = probe failed (no creds / no content_tag / network)
      //   empty missing_files     = full set present on pan123
      const pluginMissing = state.plugin.missing_files;
      const pluginExpected = state.plugin.expected_count || 0;
      if (Array.isArray(pluginMissing) && pluginMissing.length > 0) {
        pluginMetaHtml += ` <span class="text-red-600 font-semibold">· ⚠ pan123 缺失 ${pluginMissing.length} 个 plugin 文件</span>`;
        pluginTitle += '\n\nMissing on pan123:\n  ' + pluginMissing.join('\n  ');
      } else if (state.plugin.pan123_error) {
        pluginMetaHtml += ` <span class="text-amber-600 text-[11px]">· pan123 探测失败</span>`;
        pluginTitle += '\n\npan123 probe error: ' + state.plugin.pan123_error;
      } else if (Array.isArray(pluginMissing) && pluginExpected > 0) {
        pluginMetaHtml += ` <span class="text-emerald-600 text-[11px]">· pan123 ${pluginExpected}/${pluginExpected} 完整</span>`;
      }
      pluginEl.title = pluginTitle;
      pluginMeta.innerHTML = pluginMetaHtml;
    }
  } else {
    pluginEl.textContent = '-';
    pluginMeta.innerHTML = state.errors.vercel ? 'Vercel 未连接' : sourceTag('none');
  }
  // Plugin upgrade banner (release mode only — backend already filters)
  renderUpdateBanner(
    'plugin-meta', 'plugin_tag',
    (state.updates_available || {}).plugin, '上游 plugin 仓库',
  );

  // SDK
  const sdkEl = document.getElementById('sdk-version');
  const sdkMeta = document.getElementById('sdk-meta');
  if (state.sdk && state.sdk.base) {
    const base = state.sdk.base;
    const short = base.length > 26 ? base.slice(0, 23) + '...' : base;
    sdkEl.textContent = short;
    let sdkTitle = base;
    let sdkMetaHtml = `source base ${sourceTag(state.sdk.source)}`;
    // pan123 probe — non-empty missing_files means the SDK directory on
    // pan123 is incomplete (user manually deleted a file, or upload skipped).
    // pan123_error covers both "creds missing" and "network/list failure".
    const missing = state.sdk.missing_files;
    const sdkExpected = state.sdk.expected_count || 0;
    if (Array.isArray(missing) && missing.length > 0) {
      sdkMetaHtml += ` <span class="text-red-600 font-semibold">· ⚠ pan123 缺失 ${missing.length} 个 SDK 文件</span>`;
      sdkTitle = base + '\n\nMissing on pan123:\n  ' + missing.join('\n  ');
    } else if (state.sdk.pan123_error) {
      // probe didn't yield a definitive list — show as a softer warning
      sdkMetaHtml += ` <span class="text-amber-600 text-[11px]">· pan123 探测失败</span>`;
      sdkTitle = base + '\n\npan123 probe error: ' + state.sdk.pan123_error;
    } else if (Array.isArray(missing) && sdkExpected > 0) {
      // empty array = full set present; expected_count comes from publish
      // script (live), so the ratio updates automatically when new SDK
      // versions are added there.
      sdkMetaHtml += ` <span class="text-emerald-600 text-[11px]">· pan123 ${sdkExpected}/${sdkExpected} 完整</span>`;
    }
    sdkEl.title = sdkTitle;
    sdkMeta.innerHTML = sdkMetaHtml;
  } else {
    sdkEl.textContent = '-';
    sdkMeta.innerHTML = state.errors.vercel ? 'Vercel 未连接' : sourceTag('none');
  }

  // Recent releases
  renderRecentReleases(state.recent_releases, state.errors.github);

  // Overall connection badge
  const errs = [];
  if (state.errors.vercel) errs.push('Vercel');
  if (state.errors.github) errs.push('GitHub');
  if (state.errors.policy) errs.push('policy decrypt');

  if (errs.length) {
    setConn(false, `连接 OK，但以下异常: ${errs.join(' + ')}`);
  } else {
    setConn(true, 'pywebview 桥已连通 · Vercel + GitHub OK');
  }
}

function renderRecentReleases(releases, errMsg) {
  const container = document.getElementById('recent-releases');
  if (!container) return;
  if (errMsg) {
    container.innerHTML = `<div class="text-xs text-red-600 p-3 bg-red-50 rounded">GitHub 读取失败: ${errMsg}</div>`;
    return;
  }
  if (!releases || !releases.length) {
    container.innerHTML = '<div class="text-xs text-slate-500 p-3">仓库没有 release · 可通过"发布新版本"打 tag 后生成</div>';
    return;
  }
  container.innerHTML = releases.map(r => {
    const tag = r.tag || '-';
    let flag = '';
    if (r.draft) flag = '<span class="ml-2 text-xs text-amber-600">[draft]</span>';
    else if (r.prerelease) flag = '<span class="ml-2 text-xs text-purple-600">[pre]</span>';
    return `<div class="recent-release-item px-3 py-2 bg-slate-50 hover:bg-blue-50 rounded mb-1 flex justify-between items-center text-sm cursor-pointer" data-tag="${tag}" title="点击跳转到发布页并选中此 release">
      <span class="font-mono">${tag}${flag}</span>
      <span class="text-xs text-slate-500">${fmtRelativeTime(r.published_at)}</span>
    </div>`;
  }).join('');
  container.querySelectorAll('.recent-release-item').forEach(el => {
    el.addEventListener('click', () => navigateToPublishWithRelease(el.dataset.tag));
  });
}

async function navigateToPublishWithRelease(tag) {
  if (!tag) return;
  showView('publish');
  // Ensure mode=select is active
  document.querySelector('.mode-btn[data-mode="select"]')?.click();
  // Ensure source=release is active (its click handler fires loadSourceList async)
  document.querySelector('.source-btn[data-source="release"]')?.classList.add('active');
  publishState.source = 'release';
  // Load list and select
  await loadSourceList('release');
  const item = document.querySelector(`.source-item[data-tag="${tag}"]`);
  if (item) item.click();
  else {
    // Tag not in first page — tell user via execute status row
    const statusEl = document.getElementById('execute-publish-result');
    if (statusEl) statusEl.textContent = `未在列表前 N 条找到 tag ${tag},可手动翻找`;
  }
}

// Render an "⬆ 有新版本" banner under a card's meta line. The banner
// appends to the meta element so it stays inside the card layout. Clicking
// the button surgically updates only that field via update_resource_field.
function renderUpdateBanner(metaElId, fieldName, update, sourceLabel) {
  const metaEl = document.getElementById(metaElId);
  if (!metaEl) return;
  // Clear any previous banner — refreshDashboard re-renders the whole state
  metaEl.querySelectorAll('.cc-update-banner').forEach(b => b.remove());
  if (!update || !update.latest_tag) return;
  const banner = document.createElement('div');
  banner.className = 'cc-update-banner mt-1 px-2 py-1 bg-blue-50 border border-blue-200 rounded text-[11px] text-blue-700 flex items-center justify-between gap-2';
  const publishedNote = update.published_at ? ` · ${fmtRelativeTime(update.published_at)}` : '';
  banner.innerHTML =
    `<span>⬆ ${sourceLabel}有新版 <span class="font-mono font-semibold">${update.latest_tag}</span>${publishedNote}</span>` +
    `<button class="px-2 py-0.5 bg-blue-600 text-white rounded text-[11px] hover:bg-blue-700">切换</button>`;
  banner.querySelector('button').addEventListener('click', () =>
    triggerResourceUpgrade(fieldName, update.latest_tag, sourceLabel)
  );
  metaEl.appendChild(banner);
}

async function triggerResourceUpgrade(fieldName, newValue, sourceLabel) {
  if (!confirm(`确定切换${sourceLabel}到 ${newValue}?\n\n` +
               `这会立即 upsert Vercel env (NIYIEN_*),所有客户端下次拉 manifest 时会看到新版本。\n` +
               `(只更新此字段,plugin/sdk/lens 其他字段保持不变。)`)) {
    return;
  }
  try {
    const r = await pywebview.api.update_resource_field(fieldName, newValue);
    if (!r.ok) {
      alert(`切换失败: ${r.error}`);
      return;
    }
    // Reload dashboard so the card + banner re-render against new state
    refreshDashboard();
  } catch (e) {
    alert(`调用失败: ${e}`);
  }
}

async function refreshDashboard() {
  setConn(null, '正在拉取 Vercel + GitHub...');
  try {
    const state = await pywebview.api.get_dashboard_state();
    renderDashboard(state);
  } catch (e) {
    setConn(false, `桥调用失败: ${e}`);
  }
  // Auto-scan 123 网盘 inventory if user opted in (persisted in localStorage)
  if (dashPan123AutoEnabled()) {
    loadDashboardPan123Status();
  }
}

// ---- Dashboard pan123 inventory status ----

function formatPackageSize(size) {
  const n = Number(size || 0);
  if (!Number.isFinite(n) || n <= 0) return '缺 size';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

function renderPackageMetadata(packages) {
  if (!packages || !Object.keys(packages).length) {
    return '<div class="mt-1 text-[11px] text-slate-500">packages metadata: 未写入</div>';
  }
  const platformLabel = { windows: 'Windows', macos: 'macOS', linux: 'Linux', android: 'Android' };
  const rows = [];
  for (const [platform, meta] of Object.entries(packages)) {
    if (!meta || typeof meta !== 'object') continue;
    const label = platformLabel[platform] || platform;
    const parts = [];
    if ('installer_filename' in meta || 'installer_sha256' in meta || 'installer_size' in meta) {
      parts.push(renderPackageMetadataPart('installer', {
        filename: meta.installer_filename,
        size: meta.installer_size,
        sha256: meta.installer_sha256,
      }));
    }
    if ('package_filename' in meta || 'package_sha256' in meta || 'package_size' in meta) {
      parts.push(renderPackageMetadataPart('package', {
        filename: meta.package_filename,
        size: meta.package_size,
        sha256: meta.package_sha256,
      }));
    }
    rows.push(`<div><span class="font-semibold">${label}</span>: ${parts.join(' · ') || '无 package 字段'}</div>`);
  }
  return `<div class="mt-1 text-[11px] text-slate-600">${rows.join('')}</div>`;
}

function renderPackageMetadataPart(label, item) {
  const filenameOk = !!String(item.filename || '').trim();
  const sizeOk = Number(item.size || 0) > 0;
  const shaOk = /^[a-f0-9]{64}$/i.test(String(item.sha256 || '').trim());
  const filename = filenameOk ? String(item.filename).trim() : '缺 filename';
  return `${label}: ${filename} / ${sizeOk ? formatPackageSize(item.size) : '缺 size'} / ${shaOk ? 'sha256 OK' : '缺 sha256'}`;
}

function dashRenderPan123(payload) {
  const el = document.getElementById('dash-pan123-status');
  el.classList.remove('italic');
  el.classList.remove('bg-slate-50');
  const items = payload.app_versions || payload.items || [];
  const lensBundles = payload.lens_bundles || [];
  const pluginBundles = payload.plugin_bundles || [];
  const sdkStatus = payload.sdk_status || null;
  const scanError = payload.scan_error || '';

  // Summary stats: aggregate across the 4 sections.
  const sumEl = document.getElementById('dash-pan123-summary');
  const summaryParts = [];
  const appComplete = items.filter(it => it.complete).length;
  const appIncomplete = items.filter(it => !it.no_tag && it.exists && !it.complete).length;
  const appMissingDir = items.filter(it => !it.no_tag && !it.exists).length;
  if (appComplete) summaryParts.push(`<span class="text-emerald-600">App 完整 ${appComplete}</span>`);
  if (appIncomplete) summaryParts.push(`<span class="text-amber-600">App 不全 ${appIncomplete}</span>`);
  if (appMissingDir) summaryParts.push(`<span class="text-red-600">App 缺 ${appMissingDir}</span>`);
  summaryParts.push(`<span class="text-slate-600">Lens ${lensBundles.length}</span>`);
  summaryParts.push(`<span class="text-slate-600">Plugin ${pluginBundles.length}</span>`);
  if (sdkStatus) {
    const sdkBadge = sdkStatus.complete
      ? `<span class="text-emerald-600">SDK ✓</span>`
      : `<span class="text-amber-600">SDK 缺 ${(sdkStatus.files_missing || []).length}</span>`;
    summaryParts.push(sdkBadge);
  }
  sumEl.innerHTML = summaryParts.join(' · ');

  // Compose 4 sections; if app list is empty, still render lens/plugin/sdk so
  // the operator can see those even before any policy.versions is set.
  let html = '';
  if (!items.length) {
    html += '<div class="text-slate-500 mb-3">policy.versions[] 是空的,先发版本后再扫</div>';
  } else {
    html += dashRenderAppSection(items);
  }
  html += dashRenderBundleSection('Lens bundles', lensBundles,
    payload.current_lens_tag || '', 'lens');
  html += dashRenderBundleSection('Plugin bundles', pluginBundles,
    payload.current_plugin_tag || '', 'plugin');
  html += dashRenderSdkSection(sdkStatus);
  if (scanError) {
    html += `<div class="mt-3 text-red-600 text-xs px-3 py-2">扫描局部失败: ${scanError}</div>`;
  }
  el.innerHTML = html;

  // Wire up manual upload buttons (kept for app row "missing dir" cases).
  el.querySelectorAll('.dash-pan123-manual-upload-btn').forEach(btn => {
    btn.addEventListener('click', () => dashTriggerManualUpload(
      btn.dataset.tag, btn.dataset.version, parseInt(btn.dataset.runId || '0', 10),
    ));
  });
}

function dashRenderAppSection(items) {
  const itemsHtml = items.map(it => {
    const auto = it.is_auto_version
      ? '<span class="ml-2 px-1.5 py-0.5 bg-emerald-100 text-emerald-700 rounded text-[10px]">auto</span>'
      : '';
    let statusBadge, alertText = '', uploadBtn = '';
    if (it.no_tag) {
      statusBadge = '<span class="px-1.5 py-0.5 bg-slate-200 text-slate-700 rounded text-[10px]">无 tag · artifact 模式</span>';
      alertText = '⚠ 此条是用 Action artifact 发的版本(无 GitHub release tag),无法直接定位 123 网盘目录。' +
                  '需要手动跑 publish_pan123_release.py --app-source-mode=artifact --app-run-id=N 上传,或重新打 release tag。';
    } else if (!it.exists) {
      statusBadge = '<span class="px-1.5 py-0.5 bg-red-100 text-red-700 rounded text-[10px]">目录不存在</span>';
      alertText = '⚠ 123 网盘上没有此版本目录,客户端 cn 用户无法下载 — 请手动上传';
    } else if (it.complete) {
      statusBadge = '<span class="px-1.5 py-0.5 bg-emerald-100 text-emerald-700 rounded text-[10px]">完整</span>';
    } else {
      statusBadge = `<span class="px-1.5 py-0.5 bg-amber-100 text-amber-700 rounded text-[10px]">缺 ${it.files_missing.length}/${it.files_present.length + it.files_missing.length}</span>`;
      alertText = `⚠ 缺少 ${it.files_missing.join(', ')} — 上传可能失败,建议手动重试`;
    }
    if (!it.no_tag && !it.complete) {
      const runIdAttr = it.run_id ? ` data-run-id="${it.run_id}"` : '';
      const labelHint = it.run_id ? '手动上传 (artifact)' : '手动上传';
      uploadBtn = `<button class="dash-pan123-manual-upload-btn ml-2 text-xs px-2 py-1 rounded bg-blue-600 text-white hover:bg-blue-700"
                data-tag="${it.tag}" data-version="${it.version}"${runIdAttr}>${labelHint}</button>`;
    }
    const tagDisplay = it.tag || '<span class="text-slate-400">(无 tag)</span>';
    const alertLine = alertText
      ? `<div class="mt-1 text-[11px] text-amber-700">${alertText}</div>`
      : '';
    const packageLine = renderPackageMetadata(it.packages);
    return `<div class="px-3 py-2 mb-1 bg-white border border-slate-200 rounded text-xs">
      <div class="flex items-center justify-between">
        <span class="font-mono">${it.version} <span class="text-slate-400">·</span> ${tagDisplay}${auto}</span>
        <div class="flex items-center">${statusBadge}${uploadBtn}</div>
      </div>
      ${alertLine}${packageLine}
    </div>`;
  }).join('');

  return '<div class="text-[11px] text-slate-500 mb-1">App 主程序 (gyroflow-niyien-*) — 按 policy.versions[] 逐版本检查</div>' +
    itemsHtml;
}

// kind = "lens" | "plugin"; currentTag = the live NIYIEN_*_RELEASE_TAG value.
function dashRenderBundleSection(title, bundles, currentTag, kind) {
  const headerNote = `<div class="mt-4 mb-1 text-[11px] text-slate-500">${title} — `
    + (kind === 'lens'
       ? '按 <code>lens-&lt;sha12&gt;/</code> 子目录罗列'
       : '按 <code>plugin-&lt;sha12&gt;/</code> 子目录罗列')
    + (currentTag ? ` · 当前: <code class="font-mono">${currentTag}</code>` : ' · <span class="text-amber-600">未设置 current</span>')
    + `</div>`;
  if (!bundles || !bundles.length) {
    return headerNote + `<div class="text-slate-500 italic text-xs px-3 py-2">123 网盘上没有 ${kind}-* 目录</div>`;
  }
  const rows = bundles.map(b => {
    let statusBadge;
    if (b.complete) {
      statusBadge = '<span class="px-1.5 py-0.5 bg-emerald-100 text-emerald-700 rounded text-[10px]">完整</span>';
    } else {
      const total = (b.files_present || []).length + (b.files_missing || []).length;
      statusBadge = `<span class="px-1.5 py-0.5 bg-amber-100 text-amber-700 rounded text-[10px]">缺 ${(b.files_missing||[]).length}/${total}</span>`;
    }
    const currentBadge = b.is_current
      ? '<span class="ml-1 px-1.5 py-0.5 bg-emerald-600 text-white rounded text-[10px]" title="当前 manifest 指向此 bundle">current</span>'
      : '';
    const cacheBadge = b.from_cache
      ? '<span class="ml-1 px-1.5 py-0.5 bg-slate-100 text-slate-500 rounded text-[10px]">cache</span>'
      : '';
    const sizeText = b.total_size_mb >= 1
      ? `${b.total_size_mb} MB`
      : `${Math.round((b.total_size_mb || 0) * 1024)} KB`;

    const versionLines = [];
    if (kind === 'lens' && b.manifest_lens_release_tag) {
      versionLines.push(`<div><span class="text-slate-500">Lens release:</span> <span class="font-mono">${b.manifest_lens_release_tag}</span></div>`);
    }
    if (kind === 'plugin') {
      const ptag = b.manifest_plugins_release_tag || b.manifest_plugin_source_ref || '';
      if (ptag) {
        const pmode = b.manifest_plugin_source_mode ? ` (${b.manifest_plugin_source_mode})` : '';
        versionLines.push(`<div><span class="text-slate-500">Plugin source:</span> <span class="font-mono">${ptag}</span>${pmode}</div>`);
      }
    }
    if (b.manifest_generated_at) {
      versionLines.push(`<div><span class="text-slate-500">生成:</span> ${b.manifest_generated_at}</div>`);
    }
    const missingLine = (b.files_missing && b.files_missing.length)
      ? `<div class="mt-1 text-[11px] text-amber-700">⚠ 缺: ${b.files_missing.join(', ')}</div>`
      : '';
    if (b.manifest_error) {
      versionLines.push(`<div class="text-red-600">manifest 解析失败: ${b.manifest_error}</div>`);
    }
    const versionsBlock = versionLines.length
      ? `<div class="mt-1 text-[11px] space-y-0.5">${versionLines.join('')}</div>`
      : '';

    const fidLine = b.fileID ? ` <span class="text-slate-400">(fileId=${b.fileID})</span>` : '';
    const rowBg = b.is_current ? 'bg-emerald-50 border-emerald-300' : 'bg-white border-slate-200';
    return `<div class="px-3 py-2 mb-1 ${rowBg} border rounded text-xs">
      <div class="flex items-center justify-between">
        <span class="font-mono">${b.tag}${fidLine}${currentBadge}${cacheBadge}</span>
        ${statusBadge}
      </div>
      <div class="mt-1 text-[11px] text-slate-500">
        ${b.file_count || 0}/${b.expected_count || '?'} 文件 · ${sizeText}
        ${b.created_at ? ' · ' + b.created_at : ''}
      </div>
      ${missingLine}${versionsBlock}
    </div>`;
  }).join('');
  return headerNote + rows;
}

function dashRenderSdkSection(sdk) {
  const headerNote = '<div class="mt-4 mb-1 text-[11px] text-slate-500">SDK — '
    + '<code>releases/sdk/</code> 扁平目录,跨版本共享</div>';
  if (!sdk || !sdk.exists) {
    return headerNote + '<div class="text-red-600 italic text-xs px-3 py-2">123 网盘上没有 sdk/ 目录</div>';
  }
  const sizeText = sdk.total_size_mb >= 1
    ? `${sdk.total_size_mb} MB`
    : `${Math.round((sdk.total_size_mb || 0) * 1024)} KB`;
  let statusBadge;
  if (sdk.complete) {
    statusBadge = '<span class="px-1.5 py-0.5 bg-emerald-100 text-emerald-700 rounded text-[10px]">完整</span>';
  } else {
    const total = (sdk.files_present || []).length + (sdk.files_missing || []).length;
    statusBadge = `<span class="px-1.5 py-0.5 bg-amber-100 text-amber-700 rounded text-[10px]">缺 ${(sdk.files_missing||[]).length}/${total}</span>`;
  }
  const missingLine = (sdk.files_missing && sdk.files_missing.length)
    ? `<div class="mt-1 text-[11px] text-amber-700">⚠ 缺: ${sdk.files_missing.join(', ')}</div>`
    : '';
  const fidLine = sdk.fileID ? ` <span class="text-slate-400">(fileId=${sdk.fileID})</span>` : '';
  const rowBg = sdk.complete ? 'bg-white border-slate-200' : 'bg-amber-50 border-amber-300';
  return headerNote + `<div class="px-3 py-2 mb-1 ${rowBg} border rounded text-xs">
    <div class="flex items-center justify-between">
      <span class="font-mono">sdk${fidLine}</span>
      ${statusBadge}
    </div>
    <div class="mt-1 text-[11px] text-slate-500">
      ${sdk.file_count || 0}/${sdk.expected_count || '?'} 文件 · ${sizeText}
    </div>
    ${missingLine}
  </div>`;
}

async function loadDashboardPan123Status() {
  const el = document.getElementById('dash-pan123-status');
  const sumEl = document.getElementById('dash-pan123-summary');
  el.innerHTML = '<div class="text-slate-500">扫描中...</div>';
  sumEl.textContent = '';
  try {
    const r = await pywebview.api.get_pan123_inventory();
    if (!r.ok) {
      el.innerHTML = `<div class="text-red-600">扫描失败: ${r.error}</div>`;
      return;
    }
    dashRenderPan123(r);
  } catch (e) {
    el.innerHTML = `<div class="text-red-600">调用失败: ${e}</div>`;
  }
}

async function dashTriggerManualUpload(tag, version, runId) {
  if (!tag) return;
  const modeLabel = runId ? `artifact 模式 (run=${runId})` : 'release 模式';
  if (!confirm(`手动触发 ${tag} 同步到 123 网盘 [${modeLabel}]?\n\n` +
               `已存在的文件会自动跳过 (123 API 端 MD5 去重),只补上缺失的部分。\n` +
               `点确定后会跳转到"发布版本"视图,在那里看实时进度。`)) return;
  try {
    const r = await pywebview.api.start_pan123_publish_manual(tag, version || '', runId || 0);
    if (!r.ok) {
      alert(`启动失败: ${r.error}`);
      return;
    }
    showView('publish');
    document.querySelector('.mode-btn[data-mode="select"]')?.click();
    pan123StartPolling(r.token);
  } catch (e) {
    alert(`调用失败: ${e}`);
  }
}

document.getElementById('dash-pan123-refresh-btn')?.addEventListener('click', loadDashboardPan123Status);

// "自动扫" toggle — persisted in localStorage so the preference survives restart
const DASH_PAN123_AUTO_KEY = 'cc_dash_pan123_auto';
function dashPan123AutoEnabled() {
  // Default on (first launch sees no key → returns true)
  return localStorage.getItem(DASH_PAN123_AUTO_KEY) !== '0';
}
(function initDashPan123Auto() {
  const cb = document.getElementById('dash-pan123-auto');
  if (!cb) return;
  cb.checked = dashPan123AutoEnabled();
  cb.addEventListener('change', () => {
    localStorage.setItem(DASH_PAN123_AUTO_KEY, cb.checked ? '1' : '0');
    if (cb.checked) loadDashboardPan123Status();
  });
})();

// ---- View switching ----

function showView(target) {
  // Update sidebar nav highlight (only for top-level views that exist in nav)
  document.querySelectorAll('.nav-btn').forEach(b => {
    b.classList.toggle('active', b.dataset.view === target);
  });
  document.querySelectorAll('.view').forEach(v => {
    v.classList.toggle('hidden', v.dataset.view !== target);
  });
  // Prefill publish view fields on every entry — covers all paths
  // (Dashboard card, recent-release list, manual pan123 sync). Idempotent:
  // user-edited inputs are not overwritten (see prefillPublishView).
  if (target === 'publish') prefillPublishView();
}

document.querySelectorAll('.nav-btn').forEach(btn => {
  btn.addEventListener('click', () => showView(btn.dataset.view));
});

// Inline action buttons that navigate to a view (e.g. "发布新版本" card)
document.querySelectorAll('[data-action-nav]').forEach(btn => {
  btn.addEventListener('click', () => showView(btn.dataset.actionNav));
});

// ---- Publish view logic ----

const publishState = {
  mode: null,       // 'trigger' | 'tag' | 'select'
  source: null,     // 'release' | 'artifact' (only for mode=select)
  selected: null,   // selected release/artifact entry
};

// Mode switcher
document.querySelectorAll('.mode-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    const mode = btn.dataset.mode;
    publishState.mode = mode;
    document.querySelectorAll('.mode-btn').forEach(b => b.classList.toggle('active', b === btn));
    document.querySelectorAll('.mode-panel').forEach(p => {
      p.classList.toggle('hidden', p.dataset.modePanel !== mode);
    });
    if (mode === 'select' && !publishState.source) {
      // Default to release when first entering mode=select
      document.querySelector('.source-btn[data-source="release"]').click();
    }
  });
});

// ---- Mode 1: Trigger action build ----

document.getElementById('trigger-action-btn')?.addEventListener('click', async () => {
  const resultEl = document.getElementById('trigger-action-result');
  const label = document.getElementById('trigger-build-label').value.trim();
  resultEl.textContent = '触发中...';
  resultEl.className = 'text-sm text-slate-600';
  try {
    const r = await pywebview.api.trigger_action_build(label);
    if (r.ok) {
      resultEl.textContent = `✓ 已触发 · ${r.label ? 'label=' + r.label : ''}`;
      resultEl.className = 'text-sm text-emerald-600';
    } else {
      resultEl.textContent = `✗ 失败: ${r.error}`;
      resultEl.className = 'text-sm text-red-600';
    }
  } catch (e) {
    resultEl.textContent = `✗ 调用失败: ${e}`;
    resultEl.className = 'text-sm text-red-600';
  }
});

// Prefill publish view fields on every entry (called from showView).
//   Mode ① build_label ← current HEAD commit subject
//   Mode ② 3 digits  ← latest gyroflow tag patch+1 (or Cargo.toml version)
// Idempotent: build_label only fills when input is empty so user edits
// survive view switches; tag digits always refresh to latest suggestion.
async function prefillPublishView() {
  // Surface diagnosis on screen + console so silent failures are visible.
  // Old code swallowed every error which hid pywebview-not-ready / API
  // shape mismatches from users.
  const reportDebug = (msg) => {
    console.error('[prefillPublishView]', msg);
    const el = document.getElementById('trigger-action-result');
    if (el) {
      el.textContent = `[debug] ${msg}`;
      el.className = 'text-xs text-amber-600';
    }
  };
  try {
    if (typeof pywebview === 'undefined' || !pywebview.api) {
      reportDebug('pywebview.api not ready when prefill ran');
    } else {
      const r = await pywebview.api.get_head_commit_subject();
      if (!r) {
        reportDebug('get_head_commit_subject returned null/undefined');
      } else if (!r.ok) {
        reportDebug(`get_head_commit_subject err: ${r.error || '(no err msg)'}`);
      } else if (!r.subject) {
        reportDebug(`get_head_commit_subject empty subject (branch=${r.branch || '?'})`);
      } else {
        const input = document.getElementById('trigger-build-label');
        if (input && !input.value) input.value = r.subject;
      }
    }
  } catch (e) {
    reportDebug(`get_head_commit_subject threw: ${(e && e.message) || e}`);
  }
  try {
    const r = await pywebview.api.get_gyroflow_latest_tag_suggestion();
    if (r.ok) {
      document.getElementById('tag-major').value = String(r.major);
      document.getElementById('tag-minor').value = String(r.minor);
      document.getElementById('tag-patch').value = String(r.patch);
      updateTagPreview();
    } else {
      console.error('[prefillPublishView] tag suggestion err:', r.error);
    }
  } catch (e) {
    console.error('[prefillPublishView] tag suggestion threw:', e);
  }
}

// ---- Mode 2: Push tag ----

function updateTagPreview() {
  const maj = document.getElementById('tag-major').value || '0';
  const min = document.getElementById('tag-minor').value || '0';
  const pat = document.getElementById('tag-patch').value || '0';
  document.getElementById('tag-preview').textContent = `v${maj}.${min}.${pat}`;
}
['tag-major', 'tag-minor', 'tag-patch'].forEach(id => {
  document.getElementById(id)?.addEventListener('input', updateTagPreview);
});

document.getElementById('push-tag-btn')?.addEventListener('click', async () => {
  const maj = parseInt(document.getElementById('tag-major').value, 10);
  const min = parseInt(document.getElementById('tag-minor').value, 10);
  const pat = parseInt(document.getElementById('tag-patch').value, 10);
  const resultEl = document.getElementById('push-tag-result');
  if ([maj, min, pat].some(v => isNaN(v) || v < 0 || v > 999)) {
    resultEl.textContent = '✗ 版本号必须是 0-999 的数字';
    resultEl.className = 'text-sm text-red-600';
    return;
  }
  if (!confirm(`确定创建并推送 tag v${maj}.${min}.${pat} 吗?`)) return;
  resultEl.textContent = '推送中...';
  resultEl.className = 'text-sm text-slate-600';
  try {
    const r = await pywebview.api.create_and_push_tag(maj, min, pat);
    if (r.ok) {
      resultEl.textContent = `✓ tag ${r.tag} 已创建`;
      resultEl.className = 'text-sm text-emerald-600';
    } else {
      resultEl.textContent = `✗ 失败: ${r.error}`;
      resultEl.className = 'text-sm text-red-600';
    }
  } catch (e) {
    resultEl.textContent = `✗ 调用失败: ${e}`;
    resultEl.className = 'text-sm text-red-600';
  }
});

// ---- Mode 3: Select source + publish ----

const ACTION_HINTS = {
  manual_only: '只把版本加入 policy.versions[] 白名单,客户端手动查找时能找到,但不会主动推送',
  publish_and_push: '加入白名单并立即设为当前自动推送版本,所有客户端升级检查会看到这个新版',
  switch_auto: '把当前自动推送切换为此版本(版本必须已在白名单中)',
  rollback_auto: '切换自动推送到此版本(版本必须已在白名单中)',
  hide_version: '从 policy.versions[] 白名单移除此版本,如果它正在自动推送会切到其他版本',
};

// Actions that keep the "recommended" flag meaningful (manual_only / publish_and_push / switch_auto).
// rollback / hide don't take a fresh recommended state, so hide the checkbox row there.
const RECOMMENDED_VISIBLE_ACTIONS = new Set(['manual_only', 'publish_and_push', 'switch_auto']);

function syncRecommendedVisibility(actionValue) {
  const row = document.getElementById('pub-recommended')?.closest('.mb-4');
  if (!row) return;
  row.classList.toggle('hidden', !RECOMMENDED_VISIBLE_ACTIONS.has(actionValue));
}

document.getElementById('pub-action')?.addEventListener('change', (e) => {
  document.getElementById('pub-action-hint').textContent = ACTION_HINTS[e.target.value] || '';
  syncRecommendedVisibility(e.target.value);
});
// Trigger initial hint + recommended visibility
if (document.getElementById('pub-action')) {
  document.getElementById('pub-action-hint').textContent = ACTION_HINTS.manual_only;
  syncRecommendedVisibility('manual_only');
}

// Source type toggle (release | artifact)
document.querySelectorAll('.source-btn').forEach(btn => {
  btn.addEventListener('click', async () => {
    const source = btn.dataset.source;
    publishState.source = source;
    document.querySelectorAll('.source-btn').forEach(b => b.classList.toggle('active', b === btn));
    await loadSourceList(source);
  });
});

document.getElementById('reload-sources-btn')?.addEventListener('click', () => {
  if (publishState.source) loadSourceList(publishState.source);
});

async function loadSourceList(source) {
  const container = document.getElementById('source-list');
  container.innerHTML = '<div class="p-3 text-xs text-slate-500">加载中...</div>';
  try {
    const r = source === 'release'
      ? await pywebview.api.list_releases()
      : source === 'artifact'
      ? await pywebview.api.list_action_builds()
      : await pywebview.api.list_policy_orphan_versions();
    if (!r.ok) {
      container.innerHTML = `<div class="p-3 text-xs text-red-600">加载失败: ${r.error}</div>`;
      return;
    }
    const items = source === 'release' ? r.releases
                : source === 'artifact' ? r.builds
                : r.orphans;
    if (!items || !items.length) {
      const what = source === 'release' ? 'release'
                 : source === 'artifact' ? '最近 Action 构建'
                 : 'policy 残留';
      container.innerHTML = `<div class="p-3 text-xs text-slate-500">没有${what}</div>`;
      return;
    }
    container.innerHTML = items.map(item => {
      if (source === 'release') {
        const flag = item.draft ? '<span class="ml-2 text-amber-600">[draft]</span>'
                   : item.prerelease ? '<span class="ml-2 text-purple-600">[pre]</span>' : '';
        return `<div class="source-item" data-tag="${item.tag}" data-kind="release">
          <div class="font-mono">${item.tag}${flag}</div>
          <div class="text-xs text-slate-500">${fmtRelativeTime(item.published_at)}</div>
        </div>`;
      } else if (source === 'artifact') {
        const ok = item.status === 'completed' && item.conclusion === 'success';
        const badge = ok ? 'text-emerald-600' : 'text-amber-600';
        return `<div class="source-item" data-run-id="${item.run_id}" data-run-number="${item.run_number}" data-kind="artifact">
          <div class="font-mono">run #${item.run_number}</div>
          <div class="text-xs text-slate-500">${item.title || item.branch} · <span class="${badge}">${item.status}${item.conclusion ? ' · ' + item.conclusion : ''}</span></div>
        </div>`;
      } else {
        // orphan — policy.versions[] entry whose tag no longer exists on GitHub
        const autoBadge = item.is_auto_version
          ? '<span class="ml-2 text-red-600 text-xs">[当前 auto_version·禁用]</span>'
          : '';
        const tagLabel = item.tag || '(无 tag)';
        const channels = (item.channels || []).join(',') || '(空)';
        const disabled = item.is_auto_version ? 'opacity-50 pointer-events-none' : '';
        return `<div class="source-item ${disabled}" data-version="${item.version}" data-tag="${item.tag || ''}" data-changelog="${(item.changelog || '').replace(/"/g, '&quot;')}" data-kind="orphan">
          <div class="font-mono">${item.version} <span class="text-slate-400">·</span> ${tagLabel}${autoBadge}</div>
          <div class="text-xs text-slate-500">channels: ${channels}</div>
        </div>`;
      }
    }).join('');
    // Click handler
    container.querySelectorAll('.source-item').forEach(it => {
      it.addEventListener('click', () => selectSourceItem(it, items));
    });
  } catch (e) {
    container.innerHTML = `<div class="p-3 text-xs text-red-600">调用失败: ${e}</div>`;
  }
}

function selectSourceItem(el, items) {
  document.querySelectorAll('.source-item').forEach(i => i.classList.remove('active'));
  el.classList.add('active');
  const kind = el.dataset.kind;
  let entry;
  if (kind === 'release') {
    entry = items.find(x => x.tag === el.dataset.tag);
    publishState.selected = { kind: 'release', ...entry };
    // Auto-fill 3-number version from tag (strip 'v' + split by '.')
    const v = (entry.tag || '').replace(/^v/, '').split(/[-.]/);
    document.getElementById('pub-major').value = v[0] || '0';
    document.getElementById('pub-minor').value = v[1] || '0';
    document.getElementById('pub-patch').value = v[2] || '0';
    document.getElementById('pub-suffix').textContent = '';
    // Changelog fill from release body first line
    document.getElementById('pub-changelog').value = (entry.body || '').split('\n')[0] || '';
  } else if (kind === 'artifact') {
    entry = items.find(x => String(x.run_id) === el.dataset.runId);
    publishState.selected = { kind: 'artifact', ...entry };
    // Keep existing major/minor/patch, set suffix based on run_number
    document.getElementById('pub-suffix').textContent = `-0.ni.${entry.run_number}`;
    document.getElementById('pub-changelog').value = entry.title || '';
  } else if (kind === 'orphan') {
    entry = items.find(x => x.version === el.dataset.version);
    publishState.selected = { kind: 'orphan', ...entry };
    // Auto-fill version digits from entry.version (e.g. "1.6.3-0.ni.5")
    const v = String(entry.version || '').split(/[-.]/);
    document.getElementById('pub-major').value = v[0] || '0';
    document.getElementById('pub-minor').value = v[1] || '0';
    document.getElementById('pub-patch').value = v[2] || '0';
    document.getElementById('pub-suffix').textContent = '';
    document.getElementById('pub-changelog').value = entry.changelog || '';
    // Force action=hide_version (the only sensible op for an orphan entry)
    const actionSel = document.getElementById('pub-action');
    actionSel.value = 'hide_version';
    actionSel.dispatchEvent(new Event('change'));
  }
  updateFinalVersion();
  document.getElementById('execute-publish-btn').disabled = false;
  document.getElementById('execute-publish-result').textContent = '';
}

function updateFinalVersion() {
  const maj = document.getElementById('pub-major').value || '0';
  const min = document.getElementById('pub-minor').value || '0';
  const pat = document.getElementById('pub-patch').value || '0';
  const suffix = document.getElementById('pub-suffix').textContent || '';
  document.getElementById('pub-final').textContent = `${maj}.${min}.${pat}${suffix}`;
}
['pub-major', 'pub-minor', 'pub-patch'].forEach(id => {
  document.getElementById(id)?.addEventListener('input', updateFinalVersion);
});

// ---- Pan123 sync progress polling ----

const pan123State = {
  token: null,
  pollHandle: null,
  startedAt: 0,
  logBuf: [],          // last N lines, capped
};

const PHASE_LABEL = {
  resolve: '解析',
  download: '下载',
  upload: '上传',
  finalize: '完成',
};

function pan123Show() {
  document.getElementById('pan123-progress-panel').classList.remove('hidden');
}

function pan123AppendLog(line) {
  pan123State.logBuf.push(line);
  if (pan123State.logBuf.length > 400) pan123State.logBuf = pan123State.logBuf.slice(-400);
  const el = document.getElementById('pan123-log');
  if (!el) return;
  el.textContent = pan123State.logBuf.join('\n');
  el.scrollTop = el.scrollHeight;
}

function pan123RenderProgress(evt) {
  const phase = String(evt.phase || '').trim();
  const phaseTxt = PHASE_LABEL[phase] || phase || '—';
  const label = String(evt.label || '').trim();
  const message = String(evt.message || '').trim();
  const cur = Number(evt.current);
  const tot = Number(evt.total);
  const mode = String(evt.mode || '').trim();

  document.getElementById('pan123-phase-line').textContent =
    `[${phaseTxt}] ${label ? label + ' · ' : ''}${message || '...'}`;

  const bar = document.getElementById('pan123-progress-bar');
  const meta = document.getElementById('pan123-progress-meta');
  if (mode === 'indeterminate' || !Number.isFinite(tot) || tot <= 0) {
    // Indeterminate stripe — we just keep current width
    meta.textContent = '进行中...';
  } else {
    const pct = Math.min(100, Math.max(0, Math.round((cur / tot) * 100)));
    bar.style.width = `${pct}%`;
    meta.textContent = `${cur}/${tot} (${pct}%)`;
  }
}

function pan123StopPolling() {
  if (pan123State.pollHandle) {
    clearInterval(pan123State.pollHandle);
    pan123State.pollHandle = null;
  }
  pan123State.token = null;
}

function pan123Reset() {
  pan123StopPolling();
  pan123State.logBuf = [];
  pan123State.startedAt = 0;
  document.getElementById('pan123-progress-panel').classList.add('hidden');
  document.getElementById('pan123-progress-bar').style.width = '0%';
  document.getElementById('pan123-phase-line').textContent = '等待启动...';
  document.getElementById('pan123-progress-meta').textContent = '';
  document.getElementById('pan123-elapsed').textContent = '';
  document.getElementById('pan123-log').textContent = '';
}

async function pan123StartPolling(token, opts = {}) {
  pan123Reset();
  pan123Show();
  // Two-phase commit hint: tell the operator manifest is staged and will
  // only be published after this upload completes successfully.
  if (opts.stagedUntilPan123) {
    const phaseEl = document.getElementById('pan123-phase-line');
    phaseEl.innerHTML =
      '<span class="text-amber-700">⏳ manifest 等 pan123 完成才推送 (race-free 二阶段)</span>';
    pan123AppendLog('[stage] manifest is staged; will be pushed on pan123 success');
  }
  pan123State.stagedUntilPan123 = !!opts.stagedUntilPan123;
  pan123State.token = token;
  pan123State.startedAt = Date.now();
  pan123State.pollHandle = setInterval(async () => {
    if (!pan123State.token) { pan123StopPolling(); return; }
    let r;
    try {
      r = await pywebview.api.poll_publish_progress(pan123State.token);
    } catch (e) {
      pan123AppendLog(`[poll error] ${e}`);
      return;
    }
    if (!r || !r.ok) {
      pan123AppendLog(`[poll error] ${r?.error || 'unknown'}`);
      pan123StopPolling();
      return;
    }
    // Render events
    for (const evt of (r.events || [])) {
      const ts = new Date((evt.ts || Date.now() / 1000) * 1000).toLocaleTimeString();
      if (evt.type === 'progress') {
        pan123RenderProgress(evt);
        if (evt.phase || evt.message) {
          pan123AppendLog(`[${ts}] [${PHASE_LABEL[evt.phase] || evt.phase || '?'}] ${evt.label || ''} ${evt.message || ''}`.trim());
        }
      } else if (evt.type === 'log') {
        pan123AppendLog(`[${ts}] ${evt.message || ''}`);
      } else if (evt.type === 'status') {
        document.getElementById('pan123-phase-line').textContent = evt.message || '';
        pan123AppendLog(`[${ts}] STATUS: ${evt.message || ''}`);
      } else if (evt.type === 'success') {
        pan123AppendLog(`[${ts}] ✓ 同步完成`);
      } else if (evt.type === 'error') {
        pan123AppendLog(`[${ts}] ✗ 错误: ${evt.message || ''}`);
        if (evt.detail) pan123AppendLog(evt.detail);
      } else if (evt.type === 'finished') {
        pan123AppendLog(`[${ts}] ── finished ──`);
      }
    }
    // Update elapsed
    const sec = Math.floor((Date.now() - pan123State.startedAt) / 1000);
    document.getElementById('pan123-elapsed').textContent = `${sec}s`;

    if (r.finished) {
      let finalLine;
      let resultLine;  // for the inline result span next to the button
      let alertMsg = null;
      if (r.task_ok) {
        // Two-phase commit done — manifest push runs inside the runner
        // BEFORE finished=true, so summary.manifest_finalize_note is
        // set by the time we see this poll.
        const hookNote = (r.summary && r.summary.manifest_finalize_note) || '';
        const hookFailed = hookNote.toLowerCase().includes('failed');
        if (pan123State.stagedUntilPan123) {
          if (hookFailed) {
            finalLine = `✓ 同步完成 (${sec}s) · ⚠ manifest 已 upsert 但 deploy hook 失败: ${hookNote}`;
            resultLine = `✓ 123 上传完成 · ⚠ deploy hook 失败,manifest 已 upsert 但 CDN 可能未刷新`;
            alertMsg = `123 上传完成,但 deploy hook 失败:\n${hookNote}\n\n` +
                       `Vercel env 已更新,但客户端拉的 CDN manifest 可能仍是旧版,你可能要手动重新触发 deploy hook。`;
          } else {
            finalLine = `✓ 同步完成 (${sec}s) · manifest 已推送 · ${hookNote || 'cn 客户端将看到新版'}`;
            resultLine = `✓ 全部流程成功 · 123 已就绪 · manifest 已推送 (${sec}s)`;
            alertMsg = `✓ 发布完成!\n\n` +
                       `• 123 网盘已上传 (${sec}s)\n` +
                       `• manifest 已推送 · ${hookNote}\n` +
                       `• cn 客户端将立即看到新版`;
          }
        } else {
          finalLine = `✓ 同步完成 (${sec}s)`;
          resultLine = `✓ 123 同步完成 (${sec}s)`;
          alertMsg = `✓ 123 网盘同步完成 (${sec}s)`;
        }
      } else {
        finalLine = `✗ 失败: ${r.error || '未知错误'}`;
        if (pan123State.stagedUntilPan123) {
          finalLine += ' · ⚠ manifest 未推送 (策略保留旧版本)';
          resultLine = `✗ 123 同步失败 · manifest 未推送 (策略保留旧版本)`;
        } else {
          resultLine = `✗ 123 同步失败: ${r.error || '未知错误'}`;
        }
      }
      document.getElementById('pan123-phase-line').textContent = finalLine;
      // Clear the "进行中..." mid-progress text once we're done.
      document.getElementById('pan123-progress-meta').textContent = '';
      if (r.task_ok) {
        document.getElementById('pan123-progress-bar').style.width = '100%';
      }
      // Update the inline result span next to the "执行发布动作" button.
      const resultEl = document.getElementById('execute-publish-result');
      if (resultEl) {
        resultEl.textContent = resultLine;
        resultEl.className = r.task_ok ? 'text-sm text-emerald-600' : 'text-sm text-red-600';
      }
      pan123StopPolling();
      // Auto-refresh inventory after a successful sync — the new files
      // should now show up in the version list.
      if (r.task_ok) {
        setTimeout(() => loadPan123Inventory(), 500);
      }
      // Modal alert so the operator doesn't have to watch the panel —
      // many publishes take 5-30 minutes and they may switch tabs.
      if (alertMsg) {
        // Defer to next tick so the UI repaint of the success state
        // happens before the blocking alert dialog.
        setTimeout(() => alert(alertMsg), 50);
      }
    }
  }, 500);
}

document.getElementById('pan123-cancel-btn')?.addEventListener('click', async () => {
  if (!pan123State.token) return;
  if (!confirm('确定取消当前 123 同步任务?已上传的分片不会回滚。')) return;
  try {
    await pywebview.api.cancel_publish(pan123State.token);
    pan123AppendLog('[cancel] cancel signal sent');
  } catch (e) {
    pan123AppendLog(`[cancel error] ${e}`);
  }
});

// ---- Pan123 inventory scan ----

async function loadPan123Inventory() {
  const el = document.getElementById('pan123-inventory');
  el.classList.remove('italic');
  el.innerHTML = '<div class="text-slate-500">扫描中...</div>';
  try {
    const r = await pywebview.api.get_pan123_inventory();
    if (!r.ok) {
      el.innerHTML = `<div class="text-red-600">扫描失败: ${r.error}</div>`;
      return;
    }
    const items = r.items || [];
    if (!items.length) {
      el.innerHTML = '<div class="text-slate-500 italic">policy.versions[] 是空的</div>';
      return;
    }
    el.innerHTML = items.map(it => {
      const auto = it.is_auto_version
        ? '<span class="ml-2 px-1.5 py-0.5 bg-emerald-100 text-emerald-700 rounded text-[10px]">auto</span>'
        : '';
      let statusBadge, extraNote = '';
      if (it.no_tag) {
        statusBadge = '<span class="px-1.5 py-0.5 bg-slate-200 text-slate-700 rounded text-[10px]">无 tag · artifact 模式</span>';
        extraNote = '<div class="mt-1 text-amber-700">需手动跑 publish_pan123_release.py --app-source-mode=artifact 上传</div>';
      } else if (!it.exists) {
        statusBadge = '<span class="px-1.5 py-0.5 bg-red-100 text-red-700 rounded text-[10px]">目录不存在</span>';
      } else if (it.complete) {
        statusBadge = '<span class="px-1.5 py-0.5 bg-emerald-100 text-emerald-700 rounded text-[10px]">完整</span>';
      } else {
        statusBadge = `<span class="px-1.5 py-0.5 bg-amber-100 text-amber-700 rounded text-[10px]">缺 ${it.files_missing.length}/${it.files_present.length + it.files_missing.length}</span>`;
      }
      const missingList = (!it.no_tag && it.files_missing.length)
        ? `<div class="mt-1 text-amber-700">缺: ${it.files_missing.join(', ')}</div>`
        : '';
      const packageLine = renderPackageMetadata(it.packages);
      const tagDisplay = it.tag || '<span class="text-slate-400">(无 tag)</span>';
      return `<div class="px-3 py-2 mb-1 bg-white border border-slate-200 rounded">
        <div class="flex items-center justify-between">
          <span class="font-mono text-sm">${it.version} <span class="text-slate-400">·</span> ${tagDisplay}${auto}</span>
          ${statusBadge}
        </div>
        ${missingList}${extraNote}${packageLine}
      </div>`;
    }).join('');
  } catch (e) {
    el.innerHTML = `<div class="text-red-600">调用失败: ${e}</div>`;
  }
}

document.getElementById('pan123-inventory-refresh-btn')?.addEventListener('click', loadPan123Inventory);

// Execute publish action
document.getElementById('execute-publish-btn')?.addEventListener('click', async () => {
  if (!publishState.selected) return;
  const maj = document.getElementById('pub-major').value;
  const min = document.getElementById('pub-minor').value;
  const pat = document.getElementById('pub-patch').value;
  const suffix = document.getElementById('pub-suffix').textContent || '';
  // For orphan entries, use the original full version string from the policy
  // entry (it may contain pre-release/build suffix like `-0.ni.5` that the
  // 3-int form can't reconstruct). Without this, hide_version would diff
  // against a truncated key and leave the orphan in place.
  const version = publishState.selected.kind === 'orphan'
    ? publishState.selected.version
    : `${maj}.${min}.${pat}${suffix}`;
  const payload = {
    action: document.getElementById('pub-action').value,
    source_kind: publishState.selected.kind,
    version,
    changelog: document.getElementById('pub-changelog').value,
    recommended: document.getElementById('pub-recommended').checked,
    run_id: publishState.selected.run_id,
    run_number: publishState.selected.run_number,
    tag: publishState.selected.tag,
  };
  const resultEl = document.getElementById('execute-publish-result');
  if (!confirm(`确定执行 "${payload.action}" 操作,版本 ${version}?`)) return;
  resultEl.textContent = '执行中...';
  resultEl.className = 'text-sm text-slate-600';
  try {
    const r = await pywebview.api.execute_app_action(payload);
    if (r.ok) {
      resultEl.textContent = `✓ ${r.message || '已执行'}`;
      resultEl.className = 'text-sm text-emerald-600';
      // Reload list for orphan cleanup so the removed entry disappears
      if (publishState.source === 'orphan') {
        loadSourceList('orphan');
        publishState.selected = null;
        document.getElementById('execute-publish-btn').disabled = true;
      }
      // Auto-start pan123 progress polling if backend kicked off a sync task
      if (r.pan123_task_token) {
        pan123StartPolling(r.pan123_task_token, {
          stagedUntilPan123: !!r.staged_until_pan123,
        });
      } else if (r.pan123_error) {
        // Two-phase commit: pan123 didn't start, so manifest also wasn't
        // pushed. Operator needs to fix creds and retry.
        resultEl.textContent +=
          ` · ⚠ manifest 未推送 (pan123 启动失败: ${r.pan123_error})`;
      }
    } else {
      resultEl.textContent = `✗ 失败: ${r.error}`;
      resultEl.className = 'text-sm text-red-600';
    }
  } catch (e) {
    resultEl.textContent = `✗ 调用失败: ${e}`;
    resultEl.className = 'text-sm text-red-600';
  }
});

// ---- Refresh button + auto-load ----

document.getElementById('refresh-btn').addEventListener('click', refreshDashboard);

// ---- Debug buttons ----

document.getElementById('ping-btn')?.addEventListener('click', async () => {
  try { logOutput('ping →', await pywebview.api.ping()); }
  catch (e) { logOutput('ping FAILED', String(e)); }
});
document.getElementById('config-btn')?.addEventListener('click', async () => {
  try { logOutput('config →', await pywebview.api.get_config()); }
  catch (e) { logOutput('get_config FAILED', String(e)); }
});
document.getElementById('policy-btn')?.addEventListener('click', async () => {
  try { logOutput('policy →', await pywebview.api.get_current_policy()); }
  catch (e) { logOutput('get_current_policy FAILED', String(e)); }
});

// ---- Resources view ----

const resourcesState = {
  op: null,       // 'apply' | 'save'
  pluginMode: 'release',
  loaded: false,
};

function resSetPluginMode(mode) {
  resourcesState.pluginMode = mode;
  document.querySelectorAll('.plugin-mode-btn').forEach(b =>
    b.classList.toggle('active', b.dataset.pluginMode === mode)
  );
  document.querySelectorAll('.plugin-mode-panel').forEach(p =>
    p.classList.toggle('hidden', p.dataset.pluginModePanel !== mode)
  );
}

document.querySelectorAll('.plugin-mode-btn').forEach(btn => {
  btn.addEventListener('click', () => resSetPluginMode(btn.dataset.pluginMode));
});

async function loadResourcesState() {
  const statusEl = document.getElementById('resources-status');
  statusEl.textContent = '读取中...';
  statusEl.className = 'text-sm mb-4 text-slate-500';
  try {
    const r = await pywebview.api.get_resources_state();
    if (!r.ok) {
      statusEl.textContent = `失败: ${r.error}`;
      statusEl.className = 'text-sm mb-4 text-red-600';
      return;
    }
    // Populate form: prefer current Vercel envs (consistent with dashboard
    // cards), fall back to publish_defaults.
    const d = r.defaults || {};
    const cur = r.current || {};
    document.getElementById('res-lens-tag').value = cur.NIYIEN_LENS_DATA_TAG || d.lens_data_tag || '';
    document.getElementById('res-plugin-tag').value = cur.NIYIEN_PLUGINS_TAG || d.plugins_tag || '';
    document.getElementById('res-plugin-artifact').value = cur.NIYIEN_PLUGINS_ARTIFACT_NAME || d.plugins_artifact_name || '';
    document.getElementById('res-sdk-base').value = cur.NIYIEN_SDK_BASE || d.sdk_base || 'https://api.gyroflow.xyz/sdk/';
    resSetPluginMode((cur.NIYIEN_PLUGINS_SOURCE_MODE || d.plugins_source_mode || 'release').toLowerCase());

    // Render current env snapshot (read-only)
    const curEl = document.getElementById('resources-current');
    if (r.error) {
      curEl.innerHTML = `<div class="text-red-600">${r.error}</div>`;
    } else {
      const cur = r.current || {};
      const entries = Object.entries(cur).filter(([_, v]) => v);
      if (!entries.length) {
        curEl.innerHTML = '<div class="text-slate-500">Vercel 上未设置任何 NIYIEN_* 资源变量</div>';
      } else {
        curEl.innerHTML = entries.map(([k, v]) =>
          `<div><span class="text-slate-500">${k}</span> = <span class="text-slate-800">${v}</span></div>`
        ).join('');
      }
    }
    statusEl.textContent = `已载入 publish_defaults · Vercel envs snapshot`;
    statusEl.className = 'text-sm mb-4 text-slate-500';
    resourcesState.loaded = true;
  } catch (e) {
    statusEl.textContent = `调用失败: ${e}`;
    statusEl.className = 'text-sm mb-4 text-red-600';
  }
}

// Op selector
document.querySelectorAll('.res-op-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    const op = btn.dataset.resOp;
    resourcesState.op = op;
    document.querySelectorAll('.res-op-btn').forEach(b => b.classList.toggle('active', b === btn));
    document.querySelector('[data-res-form]').classList.remove('hidden');
    document.getElementById('res-form-title').textContent =
      op === 'apply' ? '立即切换 — upsert 到 Vercel envs' : '保存为下次发版默认 — 写 control_center.config.json';
    const execBtn = document.getElementById('resources-execute-btn');
    execBtn.textContent = op === 'apply' ? '立即切换' : '保存默认';
    execBtn.className = op === 'apply'
      ? 'px-4 py-2 bg-amber-600 text-white rounded-md hover:bg-amber-700'
      : 'px-4 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700';
  });
});

document.getElementById('resources-execute-btn')?.addEventListener('click', async () => {
  const op = resourcesState.op;
  if (!op) return;
  const payload = {
    lens_tag: document.getElementById('res-lens-tag').value.trim(),
    plugin_mode: resourcesState.pluginMode,
    plugin_tag: document.getElementById('res-plugin-tag').value.trim(),
    plugin_artifact_name: document.getElementById('res-plugin-artifact').value.trim(),
    sdk_base: document.getElementById('res-sdk-base').value.trim(),
  };
  const resultEl = document.getElementById('resources-execute-result');
  if (op === 'apply') {
    if (!confirm(`立即切换会 upsert 到 Vercel envs,所有客户端会立刻生效。继续?\nLens: ${payload.lens_tag}\nPlugin: ${payload.plugin_mode} ${payload.plugin_tag || payload.plugin_artifact_name || '(auto)'}\nSDK: ${payload.sdk_base}`)) return;
  }
  resultEl.textContent = '执行中...';
  resultEl.className = 'text-sm text-slate-600';
  try {
    const r = op === 'apply'
      ? await pywebview.api.apply_resources_now(payload)
      : await pywebview.api.save_resources_defaults(payload);
    if (r.ok) {
      resultEl.textContent = `✓ ${r.message}`;
      resultEl.className = 'text-sm text-emerald-600';
      // For 'apply', refresh current snapshot
      if (op === 'apply') setTimeout(loadResourcesState, 500);
    } else {
      resultEl.textContent = `✗ ${r.error}`;
      resultEl.className = 'text-sm text-red-600';
    }
  } catch (e) {
    resultEl.textContent = `✗ 调用失败: ${e}`;
    resultEl.className = 'text-sm text-red-600';
  }
});

// Scoped publish to 123 — push only lens or only plugin without re-uploading
// the other component. Backend defers the actual upload to publish_pan123_release
// with --scope=lens|plugin. Requires both NIYIEN_LENS_RELEASE_TAG and
// NIYIEN_PLUGIN_RELEASE_TAG to be seeded by a prior full publish.
async function resTriggerScopedPublish(scope, label) {
  const resultEl = document.getElementById('resources-publish-result');
  if (!confirm(`将启动「仅 ${label}」推送到 123 网盘。\n\n` +
               `当前 ${label} tag/artifact 已 upsert 到 Vercel 后才会被脚本拉取 — ` +
               `如果改了表单还没点「立即切换」,请先点。\n\n` +
               `点确定后会跳转到「发布版本」视图看实时进度。`)) return;
  resultEl.textContent = `启动中 (${label})...`;
  resultEl.className = 'text-xs text-slate-600';
  try {
    const r = await pywebview.api.start_pan123_publish_manual('', '', 0, [scope]);
    if (!r.ok) {
      resultEl.textContent = `✗ ${r.error}`;
      resultEl.className = 'text-xs text-red-600';
      return;
    }
    resultEl.textContent = `✓ 已启动 (token=${(r.token || '').slice(0, 12)}...)`;
    resultEl.className = 'text-xs text-emerald-600';
    showView('publish');
    document.querySelector('.mode-btn[data-mode="select"]')?.click();
    pan123StartPolling(r.token);
  } catch (e) {
    resultEl.textContent = `✗ 调用失败: ${e}`;
    resultEl.className = 'text-xs text-red-600';
  }
}
document.getElementById('resources-publish-lens-btn')?.addEventListener('click',
  () => resTriggerScopedPublish('lens', 'Lens'));
document.getElementById('resources-publish-plugin-btn')?.addEventListener('click',
  () => resTriggerScopedPublish('plugin', 'Plugin'));

// Full-scope publish — looks up policy.auto_version to recover the right
// app tag + run_id, then dispatches start_pan123_publish_manual without a
// scope arg (backend defaults to all three: app + lens + plugin).
document.getElementById('resources-publish-all-btn')?.addEventListener('click', async () => {
  const resultEl = document.getElementById('resources-publish-result');
  resultEl.textContent = '读取 policy.auto_version...';
  resultEl.className = 'text-xs text-slate-600';
  let tag = '', version = '', runId = 0;
  try {
    const pr = await pywebview.api.get_current_policy();
    if (!pr.ok) throw new Error(pr.error);
    const policy = pr.policy || {};
    const autoV = String(policy.auto_version || '').trim();
    const entry = (policy.versions || []).find(v => String(v.version || '') === autoV);
    if (!entry || !entry.tag) {
      resultEl.textContent = '✗ 找不到 auto_version 对应的 app tag,先在 policy 里设一个';
      resultEl.className = 'text-xs text-red-600';
      return;
    }
    tag = String(entry.tag);
    version = String(entry.version || autoV);
    runId = parseInt(entry.run_id || 0, 10) || 0;
  } catch (e) {
    resultEl.textContent = `✗ 读取 policy 失败: ${e}`;
    resultEl.className = 'text-xs text-red-600';
    return;
  }
  const modeLabel = runId ? `artifact (run=${runId})` : 'release';
  if (!confirm(`将启动「全量」推送到 123 网盘:\n\n` +
               `App tag: ${tag} [${modeLabel}]\n` +
               `Lens / Plugin 按当前 Vercel envs 拉取\n\n` +
               `已存在的文件会自动跳过 (123 API MD5 去重 + bundle hash 复用)。`)) return;
  resultEl.textContent = '启动中 (全量)...';
  try {
    const r = await pywebview.api.start_pan123_publish_manual(tag, version, runId);
    if (!r.ok) {
      resultEl.textContent = `✗ ${r.error}`;
      resultEl.className = 'text-xs text-red-600';
      return;
    }
    resultEl.textContent = `✓ 已启动 (token=${(r.token || '').slice(0, 12)}...)`;
    resultEl.className = 'text-xs text-emerald-600';
    showView('publish');
    document.querySelector('.mode-btn[data-mode="select"]')?.click();
    pan123StartPolling(r.token);
  } catch (e) {
    resultEl.textContent = `✗ 调用失败: ${e}`;
    resultEl.className = 'text-xs text-red-600';
  }
});

document.getElementById('resources-reload-btn')?.addEventListener('click', loadResourcesState);

// Auto-load when entering resources view
document.querySelector('[data-view="resources"].nav-btn')?.addEventListener('click', () => {
  if (!resourcesState.loaded) loadResourcesState();
  initResourcesExtrasOnce();
});
// Also trigger when landing via Dashboard "更新资源" card
document.querySelectorAll('[data-action-nav="resources"]').forEach(btn => {
  btn.addEventListener('click', () => {
    if (!resourcesState.loaded) loadResourcesState();
    initResourcesExtrasOnce();
  });
});

// ---- Resources extras: plugin latest run + plugin/lens tag push ----

let resourcesExtrasInitialized = false;
async function initResourcesExtrasOnce() {
  if (resourcesExtrasInitialized) return;
  resourcesExtrasInitialized = true;
  // Plugin latest run
  try {
    const r = await pywebview.api.get_plugin_latest_run();
    const box = document.getElementById('plugin-run-status');
    const detail = document.getElementById('plugin-run-detail');
    if (r.ok && r.run) {
      box.classList.remove('hidden');
      const run = r.run;
      const title = run.title ? (run.title.length > 60 ? run.title.slice(0, 60) + '...' : run.title) : '(no title)';
      detail.innerHTML = `<span class="font-mono">run #${run.run_number}</span> · ${title} · <span class="text-slate-500">${fmtRelativeTime(run.created_at)}</span> · <a href="#" data-url="${run.html_url}" class="text-emerald-700 underline">在 GitHub 查看</a>`;
      detail.querySelector('a')?.addEventListener('click', (e) => {
        e.preventDefault();
        // pywebview doesn't open links natively; rely on default browser via window.open
        try { window.open(e.target.dataset.url, '_blank'); } catch (_) {}
      });
    }
  } catch (_) { /* silent — not critical */ }

  // Lens next tag suggestion
  try {
    const r = await pywebview.api.get_lens_next_tag_suggestion();
    if (r.ok) {
      document.getElementById('lens-tag-date').value = r.date || '';
      document.getElementById('lens-tag-suffix').value = String(r.suggested_n || 1);
      updateLensTagPreview();
    }
  } catch (_) { /* silent */ }

  // Plugin latest tag suggestion (patch + 1)
  try {
    const r = await pywebview.api.get_plugin_latest_tag_suggestion();
    if (r.ok) {
      document.getElementById('plugin-tag-major').value = String(r.major);
      document.getElementById('plugin-tag-minor').value = String(r.minor);
      document.getElementById('plugin-tag-patch').value = String(r.patch);
      updatePluginTagPreview();
    }
  } catch (_) { /* silent */ }

  // Plugin trigger build_label prefill (remote default branch HEAD subject)
  try {
    const r = await pywebview.api.get_plugin_head_commit_subject();
    if (r && r.ok && r.subject) {
      const input = document.getElementById('plugin-trigger-build-label');
      if (input && !input.value) input.value = r.subject;
    }
  } catch (_) { /* silent — not critical */ }

  // Repo hints (from config)
  try {
    const r = await pywebview.api.get_config_for_edit();
    if (r.ok) {
      const cfg = r.config;
      const pluginHint = document.getElementById('plugin-repo-hint');
      const lensHint = document.getElementById('lens-repo-hint');
      if (pluginHint) pluginHint.textContent = `${cfg.plugins_owner || '?'}/${cfg.plugins_repo || '?'}`;
      if (lensHint) lensHint.textContent = `${cfg.lens_data_owner || '?'}/${cfg.lens_data_repo || '?'}`;
    }
  } catch (_) { /* silent */ }
}

// Plugin tag preview + submit
function updatePluginTagPreview() {
  const maj = document.getElementById('plugin-tag-major').value || '0';
  const min = document.getElementById('plugin-tag-minor').value || '0';
  const pat = document.getElementById('plugin-tag-patch').value || '0';
  document.getElementById('plugin-tag-preview').textContent = `v${maj}.${min}.${pat}`;
}
['plugin-tag-major', 'plugin-tag-minor', 'plugin-tag-patch'].forEach(id => {
  document.getElementById(id)?.addEventListener('input', updatePluginTagPreview);
});

document.getElementById('create-plugin-tag-btn')?.addEventListener('click', async () => {
  const maj = parseInt(document.getElementById('plugin-tag-major').value, 10);
  const min = parseInt(document.getElementById('plugin-tag-minor').value, 10);
  const pat = parseInt(document.getElementById('plugin-tag-patch').value, 10);
  const resultEl = document.getElementById('plugin-tag-result');
  if ([maj, min, pat].some(v => isNaN(v) || v < 0 || v > 999)) {
    resultEl.textContent = '✗ 3 个数字都必须是 0-999';
    resultEl.className = 'mt-2 text-xs text-red-600';
    return;
  }
  const tag = `v${maj}.${min}.${pat}`;
  if (!confirm(`确定给 plugin 仓库打 tag ${tag}?会触发 workflow 自动 build + 创建 release。`)) return;
  resultEl.textContent = '推送中...';
  resultEl.className = 'mt-2 text-xs text-slate-600';
  try {
    const r = await pywebview.api.create_plugin_tag(maj, min, pat);
    if (r.ok) {
      resultEl.innerHTML = `✓ ${r.repo} tag <code class="bg-slate-100 px-1">${r.tag}</code> 已创建`;
      resultEl.className = 'mt-2 text-xs text-emerald-700';
    } else {
      resultEl.textContent = `✗ ${r.error}`;
      resultEl.className = 'mt-2 text-xs text-red-600';
    }
  } catch (e) {
    resultEl.textContent = `✗ ${e}`;
    resultEl.className = 'mt-2 text-xs text-red-600';
  }
});

document.getElementById('trigger-plugin-action-btn')?.addEventListener('click', async () => {
  const resultEl = document.getElementById('trigger-plugin-action-result');
  const label = document.getElementById('plugin-trigger-build-label').value.trim();
  resultEl.textContent = '触发中...';
  resultEl.className = 'mt-2 text-xs text-slate-600';
  try {
    const r = await pywebview.api.trigger_plugin_action_build(label);
    if (r.ok) {
      resultEl.textContent = `✓ ${r.owner}/${r.repo} @ ${r.branch} 已触发 · label=${r.label}`;
      resultEl.className = 'mt-2 text-xs text-emerald-700';
    } else {
      resultEl.textContent = `✗ ${r.error}`;
      resultEl.className = 'mt-2 text-xs text-red-600';
    }
  } catch (e) {
    resultEl.textContent = `✗ ${e}`;
    resultEl.className = 'mt-2 text-xs text-red-600';
  }
});

// Lens tag preview + submit
function updateLensTagPreview() {
  const date = document.getElementById('lens-tag-date').value || 'YYYYMMDD';
  const suffix = document.getElementById('lens-tag-suffix').value || '1';
  document.getElementById('lens-tag-preview').textContent = `data-v${date}.${suffix}`;
}
['lens-tag-date', 'lens-tag-suffix'].forEach(id => {
  document.getElementById(id)?.addEventListener('input', updateLensTagPreview);
});

document.getElementById('create-lens-tag-btn')?.addEventListener('click', async () => {
  const date = document.getElementById('lens-tag-date').value.trim();
  const suffix = parseInt(document.getElementById('lens-tag-suffix').value, 10);
  const resultEl = document.getElementById('lens-tag-result');
  if (!/^\d{8}$/.test(date)) {
    resultEl.textContent = '✗ 日期必须是 8 位数字 YYYYMMDD';
    resultEl.className = 'mt-2 text-xs text-red-600';
    return;
  }
  if (isNaN(suffix) || suffix < 1) {
    resultEl.textContent = '✗ 序号必须 ≥ 1';
    resultEl.className = 'mt-2 text-xs text-red-600';
    return;
  }
  const tag = `data-v${date}.${suffix}`;
  if (!confirm(`确定给 lens 仓库打 tag ${tag}?会触发 workflow 自动 build + 创建 release。`)) return;
  resultEl.textContent = '推送中...';
  resultEl.className = 'mt-2 text-xs text-slate-600';
  try {
    const r = await pywebview.api.create_lens_tag(date, suffix);
    if (r.ok) {
      resultEl.innerHTML = `✓ ${r.repo} tag <code class="bg-slate-100 px-1">${r.tag}</code> 已创建`;
      resultEl.className = 'mt-2 text-xs text-emerald-700';
    } else {
      resultEl.textContent = `✗ ${r.error}`;
      resultEl.className = 'mt-2 text-xs text-red-600';
    }
  } catch (e) {
    resultEl.textContent = `✗ ${e}`;
    resultEl.className = 'mt-2 text-xs text-red-600';
  }
});

// ---- Settings view ----

function getCfgValue(cfg, dotted) {
  if (!dotted.includes('.')) return cfg[dotted] ?? '';
  const [top, sub] = dotted.split('.', 2);
  return (cfg[top] && cfg[top][sub]) ?? '';
}

async function loadSettings() {
  const statusEl = document.getElementById('settings-status');
  statusEl.textContent = '读取中...';
  statusEl.className = 'text-sm mb-4 text-slate-500';
  try {
    const r = await pywebview.api.get_config_for_edit();
    if (!r.ok) {
      statusEl.textContent = `读取失败: ${r.error}`;
      statusEl.className = 'text-sm mb-4 text-red-600';
      return;
    }
    document.querySelectorAll('.cfg-input').forEach(el => {
      const key = el.dataset.cfgKey;
      el.value = String(getCfgValue(r.config, key) || '');
    });
    // Render read-only constants
    const constsEl = document.getElementById('settings-constants');
    constsEl.innerHTML = Object.entries(r.constants).map(([k, v]) =>
      `<div><span class="text-slate-500">${k}</span> = <span class="text-slate-800">${v || '(空)'}</span></div>`
    ).join('');
    statusEl.textContent = `来自 ${r.path}`;
    statusEl.className = 'text-sm mb-4 text-slate-500';
  } catch (e) {
    statusEl.textContent = `调用失败: ${e}`;
    statusEl.className = 'text-sm mb-4 text-red-600';
  }
}

async function saveSettings() {
  const statusEl = document.getElementById('settings-status');
  const partial = {};
  document.querySelectorAll('.cfg-input').forEach(el => {
    partial[el.dataset.cfgKey] = el.value;
  });
  statusEl.textContent = '保存中...';
  statusEl.className = 'text-sm mb-4 text-slate-500';
  try {
    const r = await pywebview.api.save_config(partial);
    if (r.ok) {
      statusEl.textContent = `已保存到 ${r.path}`;
      statusEl.className = 'text-sm mb-4 text-emerald-600';
    } else {
      statusEl.textContent = `保存失败: ${r.error}`;
      statusEl.className = 'text-sm mb-4 text-red-600';
    }
  } catch (e) {
    statusEl.textContent = `调用失败: ${e}`;
    statusEl.className = 'text-sm mb-4 text-red-600';
  }
}

document.getElementById('settings-reload-btn')?.addEventListener('click', loadSettings);
document.getElementById('settings-save-btn')?.addEventListener('click', saveSettings);

// Auto-load settings when switching to that view (first time only)
let settingsLoaded = false;
document.querySelector('[data-view="settings"].nav-btn')?.addEventListener('click', () => {
  if (!settingsLoaded) {
    settingsLoaded = true;
    loadSettings();
  }
});

// ---- Stats view ----

document.getElementById('stats-fetch-btn')?.addEventListener('click', async () => {
  const days = parseInt(document.getElementById('stats-days').value, 10) || 7;
  const event = document.getElementById('stats-event').value.trim();
  const status = document.getElementById('stats-status');
  const result = document.getElementById('stats-result');
  status.textContent = '查询中...';
  try {
    const r = await pywebview.api.fetch_stats(days, event);
    if (r.ok) {
      status.textContent = `成功 · days=${days}${event ? ' · event=' + event : ''}`;
      result.textContent = JSON.stringify(r.data, null, 2);
    } else {
      status.textContent = '失败';
      result.textContent = r.error;
    }
  } catch (e) {
    status.textContent = '调用失败';
    result.textContent = String(e);
  }
});

document.getElementById('rebuild-btn')?.addEventListener('click', async () => {
  const start = document.getElementById('rebuild-start').value;
  const end = document.getElementById('rebuild-end').value;
  const result = document.getElementById('rebuild-result');
  if (!start || !end) {
    result.textContent = '开始和结束日期都必须填';
    return;
  }
  if (!confirm(`确定触发 Rebuild ${start} 到 ${end}?这会重建 telemetry 统计聚合数据。`)) return;
  result.textContent = '执行中...';
  try {
    const r = await pywebview.api.trigger_rebuild(start, end);
    result.textContent = r.ok ? JSON.stringify(r.data, null, 2) : r.error;
  } catch (e) {
    result.textContent = String(e);
  }
});

// ---- Manifest preview modal ----

function openManifestModal() {
  document.getElementById('manifest-modal').classList.remove('hidden');
}
function closeManifestModal() {
  document.getElementById('manifest-modal').classList.add('hidden');
}

document.getElementById('open-manifest-modal-btn')?.addEventListener('click', openManifestModal);
document.getElementById('close-manifest-modal-btn')?.addEventListener('click', closeManifestModal);

document.getElementById('manifest-fetch-btn')?.addEventListener('click', async () => {
  const country = document.getElementById('manifest-country').value.trim().toUpperCase() || 'CN';
  const platform = document.getElementById('manifest-platform').value;
  const status = document.getElementById('manifest-status');
  const result = document.getElementById('manifest-result');
  status.textContent = '查询中...';
  try {
    const r = await pywebview.api.preview_manifest(country, platform);
    if (r.ok) {
      status.textContent = `${country} · ${platform}`;
      result.textContent = JSON.stringify(r.data, null, 2);
    } else {
      status.textContent = '失败';
      result.textContent = r.error;
    }
  } catch (e) {
    status.textContent = '调用失败';
    result.textContent = String(e);
  }
});

// ---- Boot ----

(async () => {
  try {
    await waitForApi();
    refreshDashboard();
  } catch (e) {
    setConn(false, `${e.message}`);
  }
})();
