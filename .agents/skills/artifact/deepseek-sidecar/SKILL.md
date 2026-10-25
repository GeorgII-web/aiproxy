---
name: deepseek-sidecar
description: The DeepSeek web provider — `src/providers/deepseek.rs` talking JSON lines to the Playwright driver `sidecar/deepseek.mjs`, the automatic login flow (headless check, visible browser, back to headless), persistent profile, user agent, DOM selectors, cancel and timeout behaviour, WSL runtime paths, and the login/re-login endpoint. Use when DeepSeek answers fail, the site DOM changed, login loops, or the sidecar must be changed.
---

# DeepSeek sidecar

Rust side `DeepseekProvider` (`src/providers/deepseek.rs`) keeps one `Sidecar` (node process) behind a
`tokio::sync::Mutex`, so requests are serialized. The sidecar is started lazily on the first request and
dropped (killed) on a fatal error, timeout, or registry rebuild.

## Protocol (`sidecar/deepseek.mjs`)

| Direction | Message |
|---|---|
| in | `{"op":"chat","id","prompt","timeout_ms"}`, `{"op":"login","id"}`, `{"op":"cancel","id"}` |
| out | `{"type":"ready"}`, `{"id","type":"delta","text"}`, `{"id","type":"done"}`, `{"id","type":"error","message"}`, `{"type":"log","level","message"}` |

`Sidecar::next()` forwards `log` lines to tracing (target `deepseek`) and returns the rest; messages with a
foreign `id` are skipped. `run()` returns `Err` only when the sidecar itself is unusable (EOF, no answer within
`timeout + 30 s`); a provider `error` message is delivered to the client and keeps the sidecar.

## Login flow (automatic)

1. `ensureSession()`: launch headless, `goto` the chat URL, `loggedIn()` = URL not `sign_in`/`login` and the input
   textarea visible.
2. Not logged in → log `login needed (at <url> "<title>")`, relaunch **visible** (`headless:false`), poll every
   2 s up to 15 min until `loggedIn()`, then relaunch headless and re-check; "login did not persist" if not.
3. The first chat request without a session therefore opens the window by itself; the HTTP request waits (up to
   `timeout_secs`, Rust kills the sidecar at `timeout + 30 s`).
4. `POST /api/providers/{id}/login` (`DeepseekProvider::login`) sends `op: login` = forced visible browser, 20 min
   window; the settings page button **Log in / re-login** calls it. There is no tray item for it by design.

Before the visible browser opens, `importAuth()` is tried once per sidecar run when `AIPROXY_AUTH_FILE`
(config `auth_file`) points to a JSON with `token` and/or `cookie` (FreeDeepseekAPI's `deepseek-auth.json`):
cookies are added for the site's domain, the token is written to `localStorage.userToken` as
`{"value": token, "__version": "0"}`, the page reloads and `loggedIn()` decides. The file is a secret: keep it
outside the repo (`.gitignore` has `deepseek-auth.json`).

Cookies live in `launchPersistentContext(PROFILE)`: native runtime `<home>/deepseek-profile`, WSL runtime
`<sidecar_dir>/profile` unless `profile_dir` is set. Deleting that folder = full logout.

## Browser and user agent

- `AIPROXY_CHANNEL` (config `browser`): `chromium` default (full Chromium, new headless), `chrome`, `msedge`.
- DeepSeek's CDN answers 403 to a `HeadlessChrome` user agent, which Playwright sends even in new headless mode.
  `normalUserAgent()` therefore gives headless contexts the plain Chrome UA of the same major version; the visible
  login browser keeps its own UA. Verified: headless reaches `/sign_in` with it.
- `--disable-blink-features=AutomationControlled` is set; nothing else is spoofed.

## Chat and selectors

`SEL` at the top of the sidecar, overridable with env `AIPROXY_SELECTORS` (JSON):

| Key | Default | Meaning |
|---|---|---|
| `input` | `textarea#chat-input, textarea` | prompt box; `fill` + `Enter` sends |
| `message` | `.ds-markdown` | rendered answers; the last one is the current answer |
| `generating` | `[class*="stop" i], button[aria-label*="stop" i]` | present while generating |
| `send` | `[role="button"][class*="send" i], button[class*="send" i], button[type="submit"]` | fallback when Enter did not send |

Answer text comes from `ANSWER_TEXT` (page-side function): a clone of the answer element where every code block
(`pre`, or its `[class*="code-block"]` container with the banner "<language> Copy Download") is replaced by a
```` ```<language> ```` fence, then `innerText` of the clone attached off-screen (`opacity:0`, not
`visibility:hidden`, which `innerText` skips). A failing extraction is logged once per request (`answer text: …`).
Loop every 250 ms: new text that extends the sent prefix → `delta`; re-rendered text (prefix broken) is held
until the end and the remainder after the common prefix is sent. Done when text is stable for 1.5 s (4 s if the
generating indicator was never seen). Each request navigates to the chat URL first = new conversation
(`goHome` waits up to 8 s until no answer element of the old one is left, so `before` starts at 0).
`cancel` (client disconnected) clicks the stop control and ends the request.

Robustness (`chat` → up to 3 `attempt_` calls within the request timeout):

- Send check: after `fill` + Enter the input must empty (or an answer/generating indicator appear) within 3 s;
  otherwise Enter again, the third time the `send` button. Still nothing → error `prompt was not sent`.
- Stall: the site sometimes creates the conversation and never answers. No text and no generating indicator
  for `STALL_MS` (30 s, env `AIPROXY_STALL_MS`) → `no answer: …` warn with url/counts, new conversation, resend.
- Stuck: text unchanged for `STUCK_MS` (90 s, env `AIPROXY_STUCK_MS`) → stop control clicked; partial text already
  sent → `done`, nothing sent → resend. Every 30 s without an end a `still waiting …` warn shows the state.

## WSL runtime

`runtime = "wsl"`: `sidecar_dir` must be the Linux path of the sidecar folder (`npm install` done there, the
Playwright Chromium in `~/.cache/ms-playwright`); the Rust side cannot check Linux paths, so missing
`node_modules` surfaces as a sidecar error. The visible browser needs WSLg. Env reaches node via `/usr/bin/env`.

## Known limits

- Text comes from the rendered DOM, not the markdown source.
- Selectors are best-effort guesses; the DOM was never inspected while logged in. Fix `SEL` first when answers
  never finish or come back empty.
