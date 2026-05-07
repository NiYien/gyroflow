# NiYien Feedback Log Center

Author-side desktop tool for browsing, downloading, and triaging the
user feedback bundles that Phase 4 clients upload through niyien.com.
Built on **pywebview + Python + vanilla HTML/JS** — no Node, no bundler.

## What it does

1. Pulls the feedback index from `https://niyien.com/api/feedback/list`
   (admin-token authenticated).
2. Downloads each bundle from the appropriate backend
   (Cloudflare R2 for `region == "global"`, 123 网盘 for `region == "cn"`).
3. Auto-extracts the zip into a local cache directory you can browse.
4. One-click **Copy prompt for Codex**: substitutes the extracted bundle
   directory, user summary, app version, OS, and GPU into a markdown
   template on the system clipboard.
5. Single or batch deletion that cleans **R2/123 + Upstash KV + local
   cache** in one shot.

## Prerequisites

- **Python 3.10+**.
- `pip` reachable from your shell.
- Credentials with read+delete scope for:
  - Cloudflare R2 (the `gyroflow-feedback` bucket)
  - 123 OpenAPI (`PAN123_CLIENT_ID` / `_SECRET`)
  - Upstash Redis REST URL + token
  - The vercel `FEEDBACK_ADMIN_TOKEN` (32+ hex bytes)

## Install

```bash
cd distribution/log_center
pip install -r requirements.txt
```

`requirements.txt` pulls `pywebview`, `boto3`, `requests`, and
`pyperclip`. If you also want `.zst`-inside-zip auto-decompression
(Phase 4 client default):

```bash
pip install zstandard
```

## Configure

```bash
cp log_center.example.json log_center.config.json
# then edit log_center.config.json with your live secrets
```

The config file is gitignored. The example file stays in version
control as a schema reference. Each field maps to:

| Field                                 | Where to find it                            |
|---------------------------------------|---------------------------------------------|
| `niyien_api_base`                     | `https://niyien.com/api` for prod           |
| `feedback_admin_token`                | vercel env `FEEDBACK_ADMIN_TOKEN`           |
| `pan123.client_id` / `client_secret`  | https://www.123pan.com/developer            |
| `pan123.feedback_root_dir`            | vercel env `PAN123_FEEDBACK_ROOT` (default `/feedback`) |
| `r2.account_id`                       | Cloudflare dashboard → R2 → Account details |
| `r2.access_key_id` / `secret_access_key` | R2 → Manage R2 API Tokens                |
| `r2.bucket`                           | usually `gyroflow-feedback`                 |
| `upstash_kv.url` / `token`            | Upstash console → REST API tab              |
| `local_cache_dir`                     | relative or absolute; default `_cache/feedback` |

## Launch

**Windows (recommended)** — double-click `log_center.pyw` or:

```bat
pythonw log_center.pyw
```

**macOS / Linux:**

```bash
python log_center.py
```

To enable the embedded WebView devtools (Inspect element, console):

```bash
LOG_CENTER_DEBUG=1 python log_center.py
```

## Usage

1. **Refresh** (top right) — pulls the latest index. The default `Since`
   filter is "30 days ago"; bump it back if you need older items.
2. Use the **Region** / **State** filters to narrow the table.
3. **Download** a row → bundle goes to
   `_cache/feedback/<yyyy-mm-dd>/<id>/`. The .zip is removed; the
   extracted directory stays.
4. **Open local** — opens that directory in your OS file manager.
5. **Copy prompt** — renders `templates/analyze.md` with this row's
   data and copies the markdown to your clipboard. Paste it into Codex.
6. **Notes** — type into the textarea inside the Summary cell; it
   auto-saves to the local sqlite (never uploaded).
7. **Delete** — confirms, then removes from R2/123 + KV + local cache.
   Errors per step are reported but won't block the others.
8. **Batch delete** — check rows, click "Delete selected (N)", confirm
   the list. Sequential, not parallel.
9. **Clean** (footer) — drop extracted directories older than N days.
   sqlite rows stay; just `downloaded` flips back to 0 so you can
   re-download later.

## Cache layout

```
distribution/log_center/
  _cache/
    index.sqlite          # local index (D3)
    feedback/
      2026-05-02/
        20260502-8a2f1c3d/
          manifest.json
          logs/
            current-session.log
            incidents.log
          project/
            current.gyroflow
```

## Templates

`templates/analyze.md` is the seed Codex prompt. Edit freely — the
substituted placeholders are: `{feedback_dir}`, `{user_summary}`,
`{app_version}`, `{os}`, `{gpu}`.

## Troubleshooting

- **"Admin token rejected"** → the token in `log_center.config.json`
  doesn't match the vercel env. Rotate per
  `docs/feedback-schema.md` §8.
- **R2 SDK errors** like `ClientError: AccessDenied` → the R2 token is
  scoped to the wrong bucket or missing `Object Read` / `Delete`
  permission.
- **123 "文件不存在"** → 123 OpenAPI tokens are scoped to the same
  account as the uploader. If feedback was uploaded under different
  credentials than your tool's, you'll see this. Double-check
  `PAN123_CLIENT_ID` matches the one vercel uses.
- **Clipboard fallback to a file** → no pyperclip/pbcopy/xclip/wl-copy
  available. The path the prompt was written to appears in the toast.
- **`pywebview` window is blank on Linux** → install
  `python3-gi gir1.2-webkit2-4.1` (or `gir1.2-webkit2-4.0`).
- **Corrupt sqlite** → on next launch the tool moves
  `_cache/index.sqlite` to `index.sqlite.bak.<unix-ts>` and starts fresh
  (you'll need to Refresh again).

## Tests

```bash
pytest distribution/log_center/tests
```

The tests use mocked clients — no live R2 / 123 / niyien calls. Skipped
in CI by default (the optional end-to-end smoke test in
`tasks.md §15` requires real staging credentials).

## See also

- `openspec/changes/feedback-log-center-tool/` — full proposal /
  design / spec deltas this tool implements.
- `docs/superpowers/specs/2026-05-02-logging-and-feedback-system-design.md`
  §6 — the cross-phase design doc.
- `docs/feedback-schema.md` (in the docs repo) — server-side contract.
