---
name: cli-providers
description: How `src/providers/cli.rs` runs Claude Code (`claude -p --output-format stream-json --include-partial-messages`, prompt on stdin) and Junie (`junie --output-format=json-stream --task=`), the NDJSON events it parses, agents and extra args, timeouts, process kill rules, and how to add another CLI provider. Use when a CLI answer is wrong or empty, when changing flags, or when adding a provider kind.
---

# CLI providers

`CliProvider { cfg, flavor }` in `src/providers/cli.rs`; one process per request, no session reuse.

## Command lines

| Flavor | Command | Prompt | System prompt |
|---|---|---|---|
| Claude | `claude -p --output-format stream-json --verbose --include-partial-messages [--model m] [extra_args..]` | stdin | `--append-system-prompt` |
| Junie | `junie --skip-update-check --output-format=json-stream [--project=workdir] [--model=m] [extra_args..] --task=<prompt>` | argument | `--system-prompt=` |

- Prompts are `Launch::long_arg` (Junie `--task=`, `--system-prompt=`; Claude `--append-system-prompt`): inside
  WSL they go to temp files and a `/bin/bash -c "exec prog args.. '--task='\"$(cat f)\""` wrapper (Windows caps a
  command line at 32767 chars, goose prompts exceed it); natively on Windows a too-long line is refused with a
  clear error before spawning. Temp files are removed when the `TempFiles` guard of the launch drops.
- `extra_args` of the provider come first, then the agent's. Claude permissions: default mode, tools needing
  approval are denied; `["--dangerously-skip-permissions"]` or `["--permission-mode", "plan"]` via `extra_args`.
  A Claude Code custom agent: agent `extra_args = ["--agent", "<name>"]`.
- `workdir` = cwd; for Junie also `--project` (Junie edits files there).
- Prompt building (`openai::build_input`): system/developer messages → system; one user message → verbatim;
  otherwise `[role]\ntext` blocks joined by blank lines plus a trailing `[assistant]` header.

## Parsed events

Claude (`Parser::claude`):

| Line `type` | Action |
|---|---|
| `stream_event` with `content_block_delta` / `text_delta` | yield text, mark `streamed` |
| `assistant` (only if nothing streamed) | remember concatenated text blocks as fallback |
| `result` with `is_error` | error with `result` or `errors` |
| `result` (only if nothing streamed) | remember `result` → yielded at end |

Junie (`Parser::junie`): `step` → `tracing::info!(target: "junie")`; `result` → final text; `error` → error.
Non-JSON lines go to debug. Non-zero exit with nothing produced → `ProviderError` with the stderr tail
(last 500 chars, buffered by `process::spawn`).

## Model discovery

`CliProvider::discover()` (`discover_models = true`, default) runs at registry build in the background
(`Registry::start_discovery`, needs a Tokio context, hence the runtime handle in `AppState`) and on
`POST /api/providers/{id}/discover`:

| Flavor | How | Result |
|---|---|---|
| Junie | `junie --skip-update-check --model=__list__ --task=list models`; the list is on **stderr** as `- <id>` lines | every id Junie accepts for `--model` |
| Claude | aliases quoted in the `--model` block of `claude --help` (`fable`, `opus`, `sonnet`) plus the built-in `CLAUDE_MODELS` ids | no list command exists; update the constant when models change |

`Provider::model_ids()` = configured agents, then discovered models not already named by an agent; they resolve
through the pass-through rule. `/api/status` returns them separately as `discovered`.

## Timeouts and kill

- `timeout_secs` is a deadline on the next stdout line (`tokio::select!` with `sleep_until`); on expiry the
  stream errors and the `ChildGuard` drop kills the tree.
- Client disconnect drops the stream → same kill. Verified: no `claude` process survives an aborted request.
- Runtime `wsl`: the tree kill hits `wsl.exe`; the Linux process ends on closed pipes (claude) — Junie's JVM may
  linger until its own timeout.

## Adding a CLI provider

1. `config::ProviderKind` variant (+ `default_command`), `Registry::build` arm.
2. `Flavor` variant: args in `CliProvider::build`, parser fn returning `Event`.
3. Defaults in `config::default_providers()` only if it should exist out of the box; README table and
   `config.example.toml`.
4. Check with `../../quality-gate/build-run-check/SKILL.md`.
