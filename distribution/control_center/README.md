# NiYien Control Center (pywebview)

Native desktop UI for managing Gyroflow releases, Vercel env policy, and
Lens/Plugin/SDK version routing.

## Run

```bash
pip install requests pywebview
python distribution/control_center/control_center.py
```

Config file: `distribution/control_center/control_center.config.json`
(created from `control_center.example.json` alongside on first run).

Set env `CONTROL_CENTER_DEBUG=1` before launching to open pywebview
DevTools for debugging.

### Windows: 双击启动（无黑色 console）

双击 `control_center.pyw` 走 `pythonw.exe`，不弹 console。
首次双击如果系统没把 `.pyw` 关联到 Python，右键 → 打开方式 → 选
`pythonw.exe`（一般在 `C:\Users\<你>\AppData\Local\Programs\Python\Python3X\pythonw.exe`）
并勾选 "始终使用此应用打开"。`.py` 命令行入口保持不变。

## Layout

```
distribution/control_center/
├── control_center.py                CLI entry — `python control_center.py`
├── control_center.pyw               GUI entry — double-click, no console window
├── control_center.config.json       User config (sensitive; gitignored in practice)
├── control_center.example.json      Template config for new setups
├── control_center_setup_guide_zh.md Chinese setup walkthrough
├── README.md                        This file
├── frontend/
│   ├── index.html                   Dashboard + Publish/Hidden/Stats/Settings views
│   ├── app.js                       JS ↔ Python bridge + view logic
│   └── style.css                    Custom styles on top of Tailwind CDN
├── backend/
│   ├── api.py                       Methods exposed to JS via pywebview
│   ├── config.py                    control_center.config.json read/write
│   ├── github.py                    GitHub REST client
│   ├── vercel.py                    Vercel REST client
│   ├── git.py                       Local git operations (tag push, branch probe)
│   ├── telemetry.py                 Stats / rebuild / manifest endpoints
│   └── helpers.py                   Proxy + sensitive-field masking helpers
└── _legacy/
    └── control_center_legacy_tkinter.py   Old Tkinter UI kept for rollback
```

## Hidden Management tab

`隐藏管理` is a top-level sidebar entry alongside `发布中心`. It shows the
full `policy.versions[]` history (App rows) plus a derived list of distinct
plugin identities (Plugin rows), and lets the operator multi-select rows
to hide / unhide in one atomic submission.

Backing data:
- App hides remove the matching entry from `policy.versions[]` (sort order
  preserved). The current `auto_version` row is disabled — switch auto in
  `发布中心` first if you want to retire it.
- Plugin hides append a key to `policy.hidden_plugins[]` (a top-level
  blacklist on the policy object). Entries' `plugin_tag` fields stay
  untouched, so the hide is fully reversible by un-checking the row.
  The docs repo manifest API (`api/_control-plane.js`) consults
  `hidden_plugins[]` per-entry and falls back to defaults when matched.

The legacy `发布动作 = hide_version` dropdown in `发布中心` mode 3 is kept
for single-entry quick hides.

## Legacy Tkinter version

Retained for rollback at `_legacy/control_center_legacy_tkinter.py`.
It reads the same `control_center.config.json` at the package root
(via hardcoded `parent.parent / "control_center.config.json"`). Not
recommended for new work — the pywebview version has clearer field
discipline and fewer hidden constants. Planned removal after a few
weeks of production use.

## Known limitations

- `NIYIEN_RELEASE_POLICY_JSON` comes back encrypted from Vercel even with
  `decrypt=true`. Dashboard shows "policy 加密态 · 待 decrypt" on the App
  card. Resources (Lens/Plugin/SDK) fall back to `publish_defaults` for
  display. See `docs/superpowers/specs` or the approved plan for the fix
  paths (local policy cache / token scope upgrade / policy to GitHub).
- `execute_app_action` currently returns dry-run only — real policy
  mutation needs the decrypt path resolved first to avoid clobbering the
  live versions list.
