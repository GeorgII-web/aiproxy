---
name: build-run-check
description: How to check, build, release and smoke-test aiproxy — the gates (`cargo test`, `cargo fmt --check`, clippy with `-D warnings` for Linux and via `scripts/cargo-gnu.sh` for Windows), the static Windows exe from WSL with MinGW, installing it into the Windows profile, the GitHub release workflow on a `v*` tag, running the Linux foreground server, curl smoke tests for models, discovery, chat, streaming and error paths, and testing the Windows exe from WSL with `curl.exe`. Use when reporting a change done, building for Windows, publishing a release, or verifying something end to end.
---

# Build, run, check

## Gates before "done" (the release workflow runs the same lines)

```bash
cargo test                                               # no unit tests yet; compiles the Linux build
cargo fmt --check
cargo clippy --all-targets -- -D warnings                # Linux: `Provider` carries allow(large_enum_variant)
scripts/cargo-gnu.sh clippy --all-targets -- -D warnings # Windows code (tray)
scripts/cargo-gnu.sh build --release                     # target/x86_64-pc-windows-gnu/release/aiproxy.exe
node --check sidecar/deepseek.mjs
```

## Windows exe from WSL (no Windows toolchain)

`scripts/cargo-gnu.sh` (copied from kbswitch): on first use `apt-get download` + `dpkg -x` of the MinGW
packages into `~/.local/opt/mingw-w64` (no sudo), then cargo with `x86_64-w64-mingw32-gcc-win32` as linker and
`+crt-static`, so the exe (~2.7 MB, `lto`, `panic = "abort"`, `strip`) needs only system DLLs.
Alternative without MinGW: `cargo zigbuild --release --target x86_64-pc-windows-gnu` with `cargo-zigbuild`
and `ziglang` from pip in a venv.

## Release

`.github/workflows/release.yml` runs on a pushed tag `v*`: the gates above on `ubuntu-24.04`, the static exe,
`aiproxy-sidecar.zip` (`sidecar/deepseek.mjs` + `package.json`), then `softprops/action-gh-release` with
generated notes. Bump `version` in `Cargo.toml` first (the tray and `/api/status` show it).

```bash
git tag v0.1.0 && git push origin v0.1.0
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

Model discovery: the log shows `models discovered provider=claude count=15` and `provider=junie count=46`
about 2-20 s after start (Junie runs a JVM); `POST /api/providers/junie/discover` re-runs it. WSL runtime
checks: the log shows `wsl: claude = /home/.../claude` once per name; test resolution by hand with
`wsl.exe --exec /bin/bash -ilc 'command -v node'` (login + interactive, otherwise nvm paths are missing).

## DeepSeek checks

- Pipeline without login: a chat request must log `browser started (headless)` then `login needed (at …/sign_in …)`
  and open a window; a short `timeout_secs` (20) ends it with 502 "sidecar stopped answering" and no leftover
  `deepseek.mjs` or Chromium processes (`pgrep -af 'deepseek.mjs|chromium'`).
- Full answer path can only be verified with a logged-in profile.
