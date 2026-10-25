# AIProxy

A Windows tray app that runs a local OpenAI-compatible API in front of local AI agents. Any
OpenAI client (IDE plugins, chat UIs, scripts) talks to `http://127.0.0.1:8787/v1` and the
request is served by one of these providers:

| Provider | Kind | How | Models |
|---|---|---|---|
| Claude CLI | `claude-cli` | one `claude -p` process per request, token streaming | `claude/sonnet`, `claude/opus`, `claude/haiku`, any configured agent, or `claude/<model id>` pass-through |
| Junie CLI | `junie-cli` | one `junie --task` process per request, answer in one chunk | `junie/default`, any configured agent, or `junie/<model id>` pass-through |
| DeepSeek website | `deepseek-web` | a Playwright browser on chat.deepseek.com; first run opens a visible browser for login, then headless | `deepseek/deepseek` |

Providers and their agents are configuration, not code: an agent is a name plus a model, a
system prompt and extra CLI arguments, exposed as model `<provider id>/<agent name>`. Each
provider runs either on Windows (`runtime = "native"`) or inside WSL (`runtime = "wsl"`),
so the CLIs can live on whichever side they are installed.

Requests are stateless: the message list is flattened into one prompt (a lone user message is
passed as is; longer conversations get `[role]` headers), system messages go to the CLI's
system prompt option.

## Build (Windows)

Requirements: [Rust](https://rustup.rs), and for each provider you use: `claude` (Claude Code),
`junie` (JetBrains Junie), Node.js 20+ for DeepSeek.

```powershell
cargo build --release
# target\release\aiproxy.exe
```

For DeepSeek, install the sidecar once and keep the `sidecar\` folder next to `aiproxy.exe`
(or point `sidecar_dir` at it):

```powershell
cd sidecar
npm install          # also downloads Playwright's Chromium
```

From WSL/Linux the server part runs in the foreground (`cargo run`, Ctrl-C stops); the tray is
Windows only. `cargo check --target x86_64-pc-windows-gnu` verifies the Windows build, and the
Windows exe can be cross-built from WSL without a Windows toolchain:

```bash
python3 -m venv ~/.venvs/zigbuild && ~/.venvs/zigbuild/bin/pip install cargo-zigbuild ziglang
PATH=~/.venvs/zigbuild/bin:$PATH cargo zigbuild --release --target x86_64-pc-windows-gnu
# target/x86_64-pc-windows-gnu/release/aiproxy.exe
```

## Run

Start `aiproxy.exe`. It shows a tray icon with:

- **Open settings** - the settings page (`http://127.0.0.1:8787/`)
- **Open logs folder**
- **Restart server**
- **Start with Windows** - autostart toggle
- **Quit**

Files live in `%APPDATA%\aiproxy` (`$AIPROXY_HOME` overrides): `config.toml`, `logs\`
(one file per day), `deepseek-profile\`.

## Settings

Settings page at `/` or `config.toml` directly (see `config.example.toml` for every option):

| Section | Keys |
|---|---|
| `[server]` | `host`, `port`, `api_key` (empty = no auth; set one before binding to anything but 127.0.0.1) |
| `[app]` | `autostart` |
| `[logging]` | `level` (trace..error, switchable at runtime), `keep_files` |
| `[[providers]]` | `id`, `kind`, `enabled`, `runtime` (`native`/`wsl`), `wsl_distro`, `command`, `workdir`, `timeout_secs`, `extra_args`; DeepSeek: `url`, `browser`, `profile_dir`, `sidecar_dir` |
| `[[providers.agents]]` | `name`, `model`, `system_prompt`, `extra_args` |

Saving on the settings page applies everything live; a host or port change restarts the
listener.

### CLIs inside WSL

With `runtime = "wsl"` the command runs as `wsl.exe [-d distro] [--cd workdir] --exec ...`.
A bare command name (`claude`, `junie`, `node`) is resolved once through the WSL login shell,
so tools from nvm or `~/.local/bin` are found. `workdir` is a Linux path. For DeepSeek in WSL,
set `sidecar_dir` to the Linux path of the `sidecar` folder (after `npm install` there); the
visible login browser then needs WSLg, which Windows 11 has by default.

Claude Code runs with its default permission mode, so it cannot run tools that need
approval. Add `extra_args = ["--dangerously-skip-permissions"]` to the provider or an agent
to allow that, knowing that every HTTP client of the proxy then controls an agent on your
machine. Junie always works inside `workdir` and may edit files there.

## API

| Endpoint | |
|---|---|
| `GET /v1/models` | all models of all enabled providers |
| `GET /v1/models/{id}` | one model |
| `POST /v1/chat/completions` | chat; `"stream": true` for SSE |
| `GET /health` | `ok` |
| `GET /api/status`, `GET/PUT /api/settings`, `GET /api/logs?lines=N`, `POST /api/providers/{id}/login` | settings page backend |

```bash
curl localhost:8787/v1/models
curl localhost:8787/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"model":"claude/haiku","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

Errors use the OpenAI envelope: 404 `model_not_found`, 401 `invalid_api_key`, 400 for bad
JSON or settings, 502 `provider_error` with the CLI's message (missing binary, non-zero exit,
timeout, DeepSeek not logged in). A client that disconnects mid-stream kills the CLI process.

## DeepSeek notes

- Login is automatic: the first request without a valid session opens a visible browser
  window; log in there and the request continues headless. Cookies stay in the profile folder.
  The settings page has **Log in / re-login** per DeepSeek provider to open that window on
  demand (expired session, other account).
- The sidecar drives the website's DOM, which DeepSeek changes without notice. Selectors are
  at the top of `sidecar/deepseek.mjs` and can be overridden with the environment variable
  `AIPROXY_SELECTORS` (JSON) if the site changes.
- Streaming reads the rendered answer, so formatting is what the page shows as text.
- DeepSeek's CDN answers 403 to the `HeadlessChrome` user agent that any headless Chromium
  sends. The sidecar therefore gives headless runs the user agent of the plain Chrome of the
  same version; the visible login browser is left untouched. `browser = "chrome"` or `"msedge"`
  use an installed browser instead of Playwright's Chromium.
- Each request is a new conversation on the site; requests are served one at a time.
