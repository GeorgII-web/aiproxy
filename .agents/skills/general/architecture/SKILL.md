---
name: architecture
description: Map of the aiproxy Rust app — startup order in `src/main.rs`, `AppState` in `src/app.rs`, the provider registry and model id scheme `<provider>/<agent>`, the `Launch` runtime abstraction (native vs WSL), settings flow and file locations. Use when orienting in the code, adding a module, changing startup, settings handling or the model id scheme.
---

# Architecture

Entry point `src/main.rs`; everything else hangs off `AppState` (`src/app.rs`).

## Startup

1. `config::Paths::new()` — home dir: `$AIPROXY_HOME`, else `%APPDATA%\aiproxy` / `~/.config/aiproxy`; creates `logs/`.
2. `Settings::load_or_create()` — `config.toml`; a missing file is written with the defaults; invalid file aborts.
3. `logging::init()` — daily files `logs/aiproxy.<date>.log`, stdout too, filter reloadable.
4. `autostart::sync()` — make the OS login item match `app.autostart`.
5. Tokio runtime → `AppState::new()` → `start_server()` (bind failure is logged, app keeps running).
6. Windows: `tray::run()` owns the main thread (tao event loop). Elsewhere: wait for Ctrl-C, `shutdown()`.

## AppState

| Field | Type | Why |
|---|---|---|
| `settings` | `std::sync::RwLock<Settings>` | read from the tray thread without async |
| `registry` | `RwLock<Arc<Registry>>` | swapped as a whole on settings change; old providers drop (sidecar dies) |
| `server` | `tokio::sync::Mutex<Option<ServerRun>>` | listener task + oneshot stop for graceful restart |
| `log` | `Logging` | reload handle + log dir |

- `apply_settings(new)`: validate → save → swap → log level → rebuild registry → autostart → restart listener
  only when host or port changed (`restart_server_later`, delayed so the calling request can finish).
- Never write `config.toml` anywhere else.

## Providers and model ids

- `Registry::build()` creates one `Provider` per enabled `[[providers]]`: `Cli` (claude-cli, junie-cli) or `Deepseek`.
- Model id = `<provider id>/<agent name>`. `Registry::resolve()` is the only mapper: bare `<id>` → first agent;
  unknown agent on a CLI provider → pass-through agent with `model = name`; unknown agent on DeepSeek → none.
- `AgentConfig.tools` (`ignore` | `emulate`) switches function-calling emulation in the HTTP layer
  (`src/tools.rs`, see `../../artifact/http-api/SKILL.md`); the DeepSeek default agent has `emulate`.
- A provider returns `DeltaStream` (`Stream<Item = Result<String, ProviderError>>`). Details:
  `../../artifact/cli-providers/SKILL.md`, `../../artifact/deepseek-sidecar/SKILL.md`.

## Runtime: native vs WSL

`Launch::command()` returns `(Command, TempFiles)`: long arguments (`Arg::Long`, prompts) become temp files read
by a bash wrapper inside WSL, see `../../artifact/cli-providers/SKILL.md`.

`providers::process::Launch` describes a process (program, args, cwd, env, runtime, distro) and only
`Launch::command()` turns it into a `tokio::process::Command`:

| runtime | Result |
|---|---|
| `native` | `program args` with cwd and env |
| `wsl` (Windows host) | `wsl.exe [-d distro] [--cd cwd] --exec [/usr/bin/env K=V..] <program> args`; a bare program name is resolved once via `bash -ilc "command -v <name>"` and cached |
| `wsl` on a Linux host | treated as native, so one `config.toml` works on both sides |

`Launch::path()` converts `C:\x` to `/mnt/c/x` for WSL. Every child goes through `process::command()`:
`kill_on_drop`, `ChildGuard` kills the tree (`taskkill /T` / process group), `CREATE_NO_WINDOW`, `.exe/.cmd/.bat`
PATH lookup on Windows.

## Files at runtime

| Path | Content |
|---|---|
| `<home>/config.toml` | settings, see `config.example.toml` |
| `<home>/logs/` | daily log files, `logging.keep_files` kept |
| `<home>/deepseek-profile/` | browser cookies (native runtime) |
| `sidecar/` next to the exe or `providers[].sidecar_dir` | DeepSeek Playwright driver |
