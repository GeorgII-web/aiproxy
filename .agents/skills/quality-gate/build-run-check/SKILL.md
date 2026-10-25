---
name: build-run-check
description: How to check, build, cross-build and smoke-test aiproxy — `cargo check` for Linux and `x86_64-pc-windows-gnu`, clippy, the Windows exe via `cargo zigbuild` from WSL, installing it into the Windows profile, running the Linux foreground server, curl smoke tests for models, chat, streaming and error paths, and testing the Windows exe from WSL with `curl.exe`. Use when reporting a change done, building for Windows, or verifying something end to end.
---

# Build, run, check

## Gates before "done"

```bash
cargo check && cargo check --target x86_64-pc-windows-gnu   # Linux build and the tray code
cargo clippy                                                # accepted: "large size difference between variants"
node --check sidecar/deepseek.mjs
```

## Windows exe from WSL (no Windows toolchain)

```bash
python3 -m venv ~/.venvs/zigbuild && ~/.venvs/zigbuild/bin/pip install cargo-zigbuild ziglang   # once
PATH=~/.venvs/zigbuild/bin:$PATH cargo zigbuild --release --target x86_64-pc-windows-gnu
# target/x86_64-pc-windows-gnu/release/aiproxy.exe (~3 MB, lto + strip)
```

Install: copy to `C:\Users\<user>\AppData\Local\aiproxy\aiproxy.exe` (`/mnt/c/...` from WSL); a running exe is
locked, quit it via the tray first. Config `C:\Users\<user>\AppData\Roaming\aiproxy\config.toml`; find the
profile with `cmd.exe /c "echo %APPDATA%"` (run with cwd on `/mnt/c`, cmd.exe refuses UNC cwd).

## Run on Linux (dev)

```bash
AIPROXY_HOME=/tmp/aiproxy-dev cargo run        # foreground, Ctrl-C stops; throwaway config
curl -s localhost:8787/v1/models
curl -sN localhost:8787/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"model":"claude/haiku","stream":true,"messages":[{"role":"user","content":"Reply with exactly: ok"}]}'
curl -s localhost:8787/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"model":"junie/default","messages":[{"role":"user","content":"Do not touch files. 2+2? Number only."}]}'
```

Error paths to re-check after touching the HTTP layer: unknown model → 404 `model_not_found`; broken JSON →
400; DeepSeek without `npm install` → 502 "run `npm install` in …"; `PUT /api/settings` with duplicate ids → 400;
port change → `restart: true` and the new port answers `/health`. Stop a background instance with
`kill $(pgrep -x aiproxy)` — never `pkill -f` with a pattern that matches your own shell.

## Run the Windows exe from WSL

```bash
(setsid nohup "/mnt/c/Users/<user>/AppData/Local/aiproxy/aiproxy.exe" >/dev/null 2>&1 &)
/mnt/c/WINDOWS/system32/curl.exe -s http://127.0.0.1:8787/health          # Windows localhost, not WSL's
/mnt/c/WINDOWS/system32/tasklist.exe /FI "IMAGENAME eq aiproxy.exe"
```

WSL runtime checks: the log shows `wsl: claude = /home/.../claude` once per name; test resolution by hand with
`wsl.exe --exec /bin/bash -ilc 'command -v node'` (login + interactive, otherwise nvm paths are missing).

## DeepSeek checks

- Pipeline without login: a chat request must log `browser started (headless)` then `login needed (at …/sign_in …)`
  and open a window; a short `timeout_secs` (20) ends it with 502 "sidecar stopped answering" and no leftover
  `deepseek.mjs` or Chromium processes (`pgrep -af 'deepseek.mjs|chromium'`).
- Full answer path can only be verified with a logged-in profile.
