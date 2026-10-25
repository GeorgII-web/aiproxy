# AIProxy

A Windows tray app that runs a local OpenAI-compatible API in front of local AI agents. Any
OpenAI client (goose, IDE plugins, chat UIs, scripts) talks to `http://127.0.0.1:8787/v1` and
the request is served by one of these providers:

| Provider | Kind | How | Models |
|---|---|---|---|
| Claude CLI | `claude-cli` | one `claude -p` process per request, token streaming | `claude/sonnet`, `claude/opus`, `claude/haiku`, any configured agent, or `claude/<model id>` pass-through |
| Junie CLI | `junie-cli` | one `junie --task` process per request, answer in one chunk | `junie/default`, any configured agent, or `junie/<model id>` pass-through (Claude, GPT, Gemini, Grok ...) |
| DeepSeek website | `deepseek-web` | a Playwright browser on chat.deepseek.com; first run opens a visible browser for login, then headless; tool calls emulated | `deepseek/deepseek` |

Providers and their agents are configuration, not code: an agent is a name plus a model, a
system prompt and extra CLI arguments, exposed as model `<provider id>/<agent name>`. On top of
the agents, each CLI provider lists the CLI's own models (`discover_models`, on by default), so
every id the CLI knows works as `<provider id>/<model>`. Each provider runs either on Windows
(`runtime = "native"`) or inside WSL (`runtime = "wsl"`), so the CLIs can live on whichever
side they are installed.

Requests are stateless: the message list is flattened into one prompt (a lone user message is
passed as is; longer conversations get `[role]` headers), system messages go to the CLI's
system prompt option.

## Download

Get `aiproxy.exe` from the [latest release](https://github.com/GeorgII-web/aiproxy/releases/latest) and run it
from any folder. The exe is not signed, so Windows SmartScreen may ask once: "More info" → "Run anyway". For the
DeepSeek provider also unpack `aiproxy-sidecar.zip` into a `sidecar` folder next to the exe (or anywhere, see
`sidecar_dir`) and run `npm install` in it (Node.js 20+).

## Run

Start `aiproxy.exe`. It shows a tray icon with:

- **Open settings** - the settings page (`http://127.0.0.1:8787/`)
- **Open logs folder**
- **Restart server**
- **Start with Windows** - autostart toggle
- **Quit**

Files live in `%APPDATA%\aiproxy` (`$AIPROXY_HOME` overrides): `config.toml`, `logs\`
(one file per day), `deepseek-profile\`.

The settings page shows every provider with its agents and discovered models, a **Refresh
models** button per CLI, **Log in / re-login** for DeepSeek, the log tail, and all model ids as
one comma-separated list with a **Copy** button for clients that take a model list.

## Clients

Any client with an OpenAI-compatible provider works; base URL `http://127.0.0.1:8787/v1`, the
API key is whatever `[server].api_key` says (empty = none, any value accepted). Model ids as
listed by `GET /v1/models`, e.g. `claude/sonnet`, `junie/gpt-codex`, `deepseek/deepseek`.

goose: in its OpenAI provider set the host to `http://127.0.0.1:8787` and the base path to
`v1/chat/completions`; paste the model list from the settings page into its models field.
goose uses the Responses API for some models, which the proxy also serves.

## Settings

Settings page at `/` or `config.toml` directly (see `config.example.toml` for every option):

| Section | Keys |
|---|---|
| `[server]` | `host`, `port`, `api_key` (empty = no auth; set one before binding to anything but 127.0.0.1) |
| `[app]` | `autostart` |
| `[logging]` | `level` (trace..error, switchable at runtime), `keep_files` |
| `[[providers]]` | `id`, `kind`, `enabled`, `runtime` (`native`/`wsl`), `wsl_distro`, `command`, `workdir`, `timeout_secs`, `max_prompt_chars`, `discover_models`, `extra_args`; DeepSeek: `url`, `browser`, `profile_dir`, `sidecar_dir`, `auth_file` |
| `[[providers.agents]]` | `name`, `model`, `system_prompt`, `extra_args`, `tools` (`ignore`/`emulate`) |

Saving on the settings page applies everything live; a host or port change restarts the
listener.

### CLIs inside WSL

With `runtime = "wsl"` the command runs as `wsl.exe [-d distro] [--cd workdir] --exec ...`.
A bare command name (`claude`, `junie`, `node`) is resolved once through the WSL login shell,
so tools from nvm or `~/.local/bin` are found. `workdir` is a Linux path. Prompts are passed
through temp files and a `bash` wrapper, because Windows caps a command line at 32767
characters and Junie takes the task as an argument. For DeepSeek in WSL, set `sidecar_dir` to
the Linux path of the `sidecar` folder (after `npm install` there); the visible login browser
then needs WSLg, which Windows 11 has by default.

Claude Code runs with its default permission mode, so it cannot run tools that need
approval. Add `extra_args = ["--dangerously-skip-permissions"]` to the provider or an agent
to allow that, knowing that every HTTP client of the proxy then controls an agent on your
machine. Junie always works inside `workdir` and may edit files there.

## API

| Endpoint | |
|---|---|
| `GET /v1/models` | all models of all enabled providers |
| `GET /v1/models/{id}` | one model |
| `POST /v1/chat/completions` | chat; `"stream": true` for SSE; `tools` with tool emulation |
| `POST /v1/responses` | the Responses API: `input` string or items, `instructions`, `tools`; `"stream": true` for SSE events |
| `GET /health` | `ok` |
| `GET /api/status`, `GET/PUT /api/settings`, `GET /api/logs?lines=N`, `POST /api/providers/{id}/login`, `POST /api/providers/{id}/discover` | settings page backend |

```bash
curl localhost:8787/v1/models
curl localhost:8787/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"model":"claude/haiku","stream":true,"messages":[{"role":"user","content":"hi"}]}'
curl localhost:8787/v1/responses -H 'Content-Type: application/json' \
  -d '{"model":"deepseek/deepseek","input":"hi"}'
```

Errors use the OpenAI envelope: 404 `model_not_found`, 401 `invalid_api_key`, 400 for bad
JSON, settings or the limits below, 502 `provider_error` with the CLI's message (missing
binary, non-zero exit, timeout, DeepSeek not logged in). A client that disconnects mid-stream
kills the CLI process. Unknown routes answer 404 with a hint to the right endpoint.

## Limits

What a provider cannot do is rejected by the proxy with a 400 before anything is sent, so a
client sees the reason at once instead of a slow failure:

| Check | Error code | Setting |
|---|---|---|
| prompt longer than the provider accepts (system + flattened messages, characters) | `context_length_exceeded` | `max_prompt_chars` per provider, 0 = no limit; DeepSeek default 400000 (the site took 500k characters of random text in tests, so this is a safety margin below the model context, not a site limit) |
| non-text content parts (`image_url`, `input_image`, audio, files) | `unsupported_content` | none: no provider takes them |

Not checked: `tools` on a CLI provider are ignored (the CLI has its own tools), DeepSeek serves
one request at a time (others wait), and every request is a new stateless conversation. A CLI
running natively on Windows gets a 502 with a clear message when the prompt would not fit the
command line (see CLIs inside WSL).

## Tool calls

The CLIs are agents with their own tools, so a client's `tools` are ignored for them (the CLI
runs its own commands). A plain chat model like DeepSeek gets tool calling emulated when the
agent has `tools = "emulate"` (the default for the DeepSeek agent): the client's function tools
go into the system prompt with a call format, a call in the answer comes back as a real OpenAI
tool call (`tool_calls` / `finish_reason: "tool_calls"`, or a `function_call` item in the
Responses API), the client runs the tool and sends the result, and the next request continues
from there. The proxy never runs tools itself. With emulation the answer is not streamed
incrementally (it has to be parsed first). If the model breaks the format the text is returned
as a normal answer.

## DeepSeek notes

- Login is automatic: the first request without a valid session opens a visible browser
  window; log in there and the request continues headless. Cookies stay in the profile folder.
  The settings page has **Log in / re-login** per DeepSeek provider to open that window on
  demand (expired session, other account).
- An existing session can be imported instead: `auth_file` = path to a JSON with `token`
  (DeepSeek's `userToken`) and/or `cookie` (a `Cookie` header string), the format of
  FreeDeepseekAPI's `deepseek-auth.json`. It is tried once before the login window opens.
- The sidecar drives the website's DOM, which DeepSeek changes without notice. Selectors are
  at the top of `sidecar/deepseek.mjs` and can be overridden with the environment variable
  `AIPROXY_SELECTORS` (JSON) if the site changes.
- Streaming reads the rendered answer; code blocks are put back into markdown fences with
  their language, the rest is the page's text.
- DeepSeek's CDN answers 403 to the `HeadlessChrome` user agent that any headless Chromium
  sends. The sidecar therefore gives headless runs the user agent of the plain Chrome of the
  same version; the visible login browser is left untouched. `browser = "chrome"` or `"msedge"`
  use an installed browser instead of Playwright's Chromium.
- Each request is a new conversation on the site; requests are served one at a time. The site
  sometimes takes a prompt and never answers; after 30 s without an answer the sidecar sends it
  again in a new conversation (3 attempts), and an answer that stops changing for 90 s is cut off.
- Typical latency is about 10 s per request: roughly 4 s is the model, the rest the browser flow
  (new chat, polling, a stability wait at the end).

## Build

Requirements: [Rust](https://rustup.rs), and for each provider you use: `claude` (Claude Code),
`junie` (JetBrains Junie), Node.js 20+ for DeepSeek.

On Windows:

```powershell
cargo build --release
# target\release\aiproxy.exe
cd sidecar && npm install      # DeepSeek only; also downloads Playwright's Chromium
```

From WSL/Linux the server part runs in the foreground (`cargo run`, Ctrl-C stops); the tray is
Windows only. The static Windows exe is cross-built from WSL without a Windows toolchain
(MinGW is unpacked into `~/.local/opt/mingw-w64` on first use, no sudo):

```bash
rustup target add x86_64-pc-windows-gnu
scripts/cargo-gnu.sh build --release   # target/x86_64-pc-windows-gnu/release/aiproxy.exe
```

Checks before a commit: `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
(also through `scripts/cargo-gnu.sh` for the Windows target), `node --check sidecar/deepseek.mjs`.

## Release

Bump `version` in `Cargo.toml`, commit, then push a tag `v<version>`. `.github/workflows/release.yml`
runs the checks, builds the static exe and publishes a GitHub release with `aiproxy.exe` and
`aiproxy-sidecar.zip` attached.

```bash
git tag -a v0.2.0 -m "0.2.0" && git push origin v0.2.0
```
