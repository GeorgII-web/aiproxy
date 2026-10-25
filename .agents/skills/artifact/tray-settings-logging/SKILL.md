---
name: tray-settings-logging
description: The Windows tray (`src/tray.rs`, tao + tray-icon, menu items and refresh on click), the settings page `ui/index.html` (fields, providers TOML editor, log tail, DeepSeek login button), autostart via `auto-launch` (`src/autostart.rs`) and logging (`src/logging.rs`, daily files, runtime level reload). Use when changing the menu, the settings page, autostart or log files.
---

# Tray, settings page, autostart, logging

## Tray (Windows only, `src/tray.rs`)

- `cfg(windows)` module and dependencies (`tao`, `tray-icon` under `[target.'cfg(windows)'.dependencies]`);
  Linux builds have no tray and run the server in the foreground.
- The tray icon is created inside the event loop on `StartCause::Init` (required on Windows). The icon is drawn in
  code (`icon()`: blue disc, white ring), no asset file.
- Menu: status line (disabled, `AIProxy <version> · <base url>/v1`), Open settings (browser to base URL), Open logs
  folder, Restart server, Start with Windows (check item), Quit. No DeepSeek item: login is automatic and on the
  settings page.
- Any `TrayIconEvent` refreshes the status text, the tooltip and the autostart check state, so a settings change
  shows up on the next click.
- Menu actions that need async use `rt.spawn`; Quit runs `shutdown()` with `rt.block_on` and exits.
- Release builds set `windows_subsystem = "windows"` (no console); child processes use `CREATE_NO_WINDOW`.

## Settings page (`ui/index.html`)

- Single file, vanilla JS, served at `/`; talks to `/api/*` (see `../http-api/SKILL.md`).
- Sections: Status (providers table with models and a **Log in / re-login** button per `deepseek-web` row),
  Models for clients (readonly textarea `models_csv`: all models of the enabled providers, agents then discovered,
  comma separated, with a **Copy** button; built from `/api/status` for clients like goose that take a model list),
  Server (host, port, api key, log level, keep files, autostart), Providers (TOML textarea), Log (200 lines,
  Refresh).
- Save & apply sends `PUT /api/settings`; on `restart: true` the page redirects to the new base URL after 1.5 s.

## Autostart (`src/autostart.rs`)

`auto-launch` with app name `AIProxy` and the current exe path (registry Run key on Windows). `sync(wanted)` at
startup repairs the entry when the exe moved; `AppState::set_autostart` goes through `apply_settings`.

## Logging (`src/logging.rs`)

- `tracing_appender` daily rotation, prefix `aiproxy`, suffix `log`, `max_log_files = logging.keep_files`;
  non-blocking writer, plus a stdout layer.
- Filter `"<level>,hyper_util=warn,h2=warn,tower_http=warn"` in a `reload::Layer`; `Logging::set_level()` swaps it
  live (called from `apply_settings`).
- `Logging::tail(n)` reads the lexicographically newest `aiproxy.*.log`.
- Targets worth filtering: `deepseek` (sidecar logs), `junie` (steps), `stderr` / `stdout` (raw CLI lines, debug).
