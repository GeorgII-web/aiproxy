# aiproxy

Rust tray app (Windows) serving an OpenAI-compatible API on top of local AI agents. README.md is the user
manual; this file is for agents working on the code.

## Layout

| Path | Role |
|---|---|
| `src/main.rs` | startup: paths → settings → logging → server → tray (Windows) or Ctrl-C wait |
| `src/config.rs` | `Settings` (TOML), `Paths`, provider/agent config, validation |
| `src/app.rs` | `AppState`: settings, provider registry, start/restart/stop of the HTTP server, apply settings |
| `src/server/` | axum router; `v1.rs` OpenAI endpoints, `api.rs` settings page backend, auth + request log |
| `src/openai.rs` | request/response types, message flattening (`build_input`) |
| `src/providers/` | `Registry` + `Provider` enum; `cli.rs` Claude/Junie, `deepseek.rs` sidecar client, `process.rs` spawning |
| `src/tray.rs` | Windows-only tray menu (tao + tray-icon) |
| `src/logging.rs`, `src/autostart.rs` | daily log files with runtime level reload; registry Run key |
| `ui/index.html` | settings page, served at `/` (embedded with `include_str!`) |
| `sidecar/deepseek.mjs` | Playwright driver for chat.deepseek.com, JSON lines over stdio |

## Skills (`.agents/skills`, read the one that matches before working)

| Skill | Read when |
|---|---|
| [architecture](.agents/skills/general/architecture/SKILL.md) | orienting, startup, `AppState`, model ids, runtime native/WSL, settings flow, file locations |
| [cli-providers](.agents/skills/artifact/cli-providers/SKILL.md) | Claude/Junie command lines, parsed events, timeouts, adding a CLI provider |
| [deepseek-sidecar](.agents/skills/artifact/deepseek-sidecar/SKILL.md) | DeepSeek protocol, automatic login, profile, user agent, selectors, WSL paths |
| [http-api](.agents/skills/artifact/http-api/SKILL.md) | endpoints, SSE, error envelope, auth, settings backend |
| [tray-settings-logging](.agents/skills/artifact/tray-settings-logging/SKILL.md) | tray menu, settings page, autostart, log files |
| [build-run-check](.agents/skills/quality-gate/build-run-check/SKILL.md) | checks, Windows cross-build and install, running and smoke tests |

## Rules

- Model id = `<provider id>/<agent name>`; `Registry::resolve` is the single place that maps it.
- A provider returns a `DeltaStream`; the HTTP layer awaits the first delta before answering so provider
  failures become real HTTP errors (502) instead of errors inside an SSE stream.
- Every child process goes through `providers::process::Launch` → `command` (kill on drop, process tree
  kill, no console window on Windows, PATH lookup of `.cmd`/`.bat`). `Launch` is runtime-agnostic:
  `runtime = wsl` turns it into `wsl.exe --exec [/usr/bin/env K=V..] <resolved program> args`.
- Windows exe from WSL: `cargo zigbuild --release --target x86_64-pc-windows-gnu` (venv `~/.venvs/zigbuild`).
- Settings changes go through `AppState::apply_settings` (validate → save → apply); never write the file
  elsewhere.
- Tray is `cfg(windows)` only; everything else must keep compiling on Linux (`cargo check`) and for Windows
  (`cargo check --target x86_64-pc-windows-gnu`).
- DeepSeek DOM selectors live only in the sidecar (`SEL`), overridable via `AIPROXY_SELECTORS`.

## Checks

```bash
cargo check && cargo check --target x86_64-pc-windows-gnu
node --check sidecar/deepseek.mjs
cargo run    # Linux: foreground server, Ctrl-C stops; AIPROXY_HOME=/tmp/x for a throwaway config
```
