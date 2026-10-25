---
name: http-api
description: The axum HTTP layer in `src/server` — OpenAI endpoints `/v1/models`, `/v1/models/{*id}`, `/v1/chat/completions` and `/v1/responses` with SSE streaming, the first-delta wait that turns provider failures into 502, the error envelope, Bearer auth, request log line, and the settings backend `/api/status`, `/api/settings`, `/api/logs`, `/api/providers/{id}/login`. Use when changing or debugging an endpoint, response shape, auth, or the settings page backend.
---

# HTTP API

Router in `src/server/mod.rs`; OpenAI handlers `src/server/v1.rs`; settings backend `src/server/api.rs`;
types in `src/openai.rs`.

## Endpoints

| Route | Handler | Notes |
|---|---|---|
| `GET /` | `ui` | `ui/index.html` embedded with `include_str!` |
| `GET /health` | inline | `ok` |
| `GET /v1/models` | `v1::models` | all `Registry::models()` |
| `GET /v1/models/{*id}` | `v1::model` | wildcard because ids contain `/` |
| `POST /v1/chat/completions` | `v1::chat` | `stream: true` → SSE |
| `POST /v1/responses` | `v1::responses` | Responses API (goose uses it for some models): `ResponsesRequest::messages()` turns `instructions` + `input` (string, `message`, `function_call`, `function_call_output` items) into chat messages; non-stream → one `response` object with one `output_text` message; stream → named SSE events `response.created` … `response.output_text.delta` … `response.completed`, `error` + `response.failed` on a provider error. No tools. |
| `GET /api/status` | `api::status` | version, base_url, paths, autostart, providers with models |
| `GET/PUT /api/settings` | `api::get_settings` / `put_settings` | `SettingsView` = server, app, logging + `providers_toml` string |
| `GET /api/logs?lines=N` | `api::logs` | tail of the newest log file, 1..5000 lines |
| `POST /api/providers/{id}/login` | `api::login` | DeepSeek forced login, blocks until done |

Layers (outer to inner): permissive CORS, `log_request` (`method path status ms` at info), `auth`.

## Chat handling

1. Bad JSON → 400 envelope (`JsonRejection` text). Unknown model → 404 `model_not_found`.
2. `build_input` flattens messages; `Resolved::chat` gives a `DeltaStream`.
3. The handler awaits the **first** delta before answering: a provider error here is a 502 `provider_error`
   with the CLI message. Only afterwards the SSE or the collected JSON is produced.
4. SSE: first chunk `delta.role = assistant`, then content chunks, then `finish_reason: stop`, then `[DONE]`.
   A mid-stream error becomes a `{"error": …}` event followed by `[DONE]`. Keep-alive pings are on.
5. Non-stream: `usage` is an estimate (chars / 4); the CLIs report nothing.

## Errors and auth

- Envelope `{"error":{"message","type","param","code"}}` via `server::error()`. Codes used: `model_not_found`,
  `invalid_api_key`, `provider_error`.
- `server.api_key` non-empty → `Authorization: Bearer <key>` required on `/v1*` and `/api*`; `/` and `/health`
  stay open. The settings page stores the key in `localStorage` after a 401 prompt.

## Settings PUT

`providers_toml` is parsed with `Settings::parse_providers_toml` (a `[[providers]]` document), merged with the
scalar sections and passed to `AppState::apply_settings`. Response `{"restart": bool, "base_url"}`; the page
redirects to the new base URL when `restart` is true.
