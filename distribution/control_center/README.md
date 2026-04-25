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
│   ├── index.html                   Dashboard + Publish/Resources/Stats/Settings views
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
