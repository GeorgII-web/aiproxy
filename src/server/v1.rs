//! OpenAI-compatible endpoints.

use std::convert::Infallible;
use std::sync::Arc;

use async_stream::stream;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde_json::json;

use super::error;
use crate::app::AppState;
use crate::openai::*;
use crate::providers::{DeltaStream, ProviderError};

pub async fn models(State(state): State<Arc<AppState>>) -> Json<ModelList> {
    let registry = state.registry.read().unwrap().clone();
    let data = registry.models().iter().map(|id| Model::new(id)).collect();
    Json(ModelList {
        object: "list",
        data,
    })
}

pub async fn model(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let registry = state.registry.read().unwrap().clone();
    match registry.resolve(&id) {
        Some(r) => Json(Model::new(&r.model_id)).into_response(),
        None => not_found(&id),
    }
}

fn not_found(model: &str) -> Response {
    error(
        StatusCode::NOT_FOUND,
        format!("model {model:?} not found; see GET /v1/models"),
        "invalid_request_error",
        Some("model_not_found"),
    )
}

fn upstream(e: &ProviderError) -> Response {
    error(
        StatusCode::BAD_GATEWAY,
        e.to_string(),
        "server_error",
        Some("provider_error"),
    )
}

fn bad_request(e: JsonRejection) -> Response {
    error(
        StatusCode::BAD_REQUEST,
        e.body_text(),
        "invalid_request_error",
        None,
    )
}

/// A started chat: the resolved model id, the prompt (for the usage estimate) and the deltas.
struct Started {
    model: String,
    prompt: String,
    deltas: DeltaStream,
}

/// Resolves the model and waits for the first delta so that a provider failure is a real HTTP error.
async fn start(
    state: &AppState,
    model: &str,
    messages: &[Message],
    stream: bool,
) -> Result<Started, Box<Response>> {
    let registry = state.registry.read().unwrap().clone();
    let Some(resolved) = registry.resolve(model) else {
        return Err(Box::new(not_found(model)));
    };
    let input = build_input(messages);
    tracing::info!(model = %resolved.model_id, prompt_chars = input.prompt.len(), stream, "chat");
    let prompt = input.prompt.clone();
    let mut deltas = resolved.chat(input);
    let first = match deltas.next().await {
        Some(Err(e)) => {
            tracing::warn!(model = %resolved.model_id, "{e}");
            return Err(Box::new(upstream(&e)));
        }
        Some(Ok(t)) => Some(t),
        None => None,
    };
    Ok(Started {
        model: resolved.model_id.clone(),
        prompt,
        deltas: Box::pin(futures::stream::iter(first.map(Ok)).chain(deltas)),
    })
}

async fn collect(model: &str, mut deltas: DeltaStream) -> Result<String, Box<Response>> {
    let mut content = String::new();
    while let Some(item) = deltas.next().await {
        match item {
            Ok(t) => content.push_str(&t),
            Err(e) => {
                tracing::warn!(model, "{e}");
                return Err(Box::new(upstream(&e)));
            }
        }
    }
    Ok(content)
}

pub async fn chat(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return bad_request(e),
    };
    let Started {
        model,
        prompt,
        deltas,
    } = match start(&state, &req.model, &req.messages, req.stream).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let id = new_id();
    let created = now();
    if req.stream {
        return sse(id, created, model, deltas);
    }
    let content = match collect(&model, deltas).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let usage = Usage::estimate(&prompt, &content);
    let response = ChatResponse {
        id,
        object: "chat.completion",
        created,
        model,
        choices: vec![Choice {
            index: 0,
            message: AssistantMessage {
                role: "assistant",
                content,
            },
            finish_reason: "stop",
        }],
        usage,
    };
    Json(response).into_response()
}

/// `POST /v1/responses`: the Responses API with one `output_text` message, no tools.
pub async fn responses(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ResponsesRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return bad_request(e),
    };
    let messages = req.messages();
    let Started {
        model,
        prompt,
        deltas,
    } = match start(&state, &req.model, &messages, req.stream).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let ids = ResponseIds {
        response: id("resp"),
        message: id("msg"),
        created: now(),
        model,
    };
    if req.stream {
        return responses_sse(ids, prompt, deltas);
    }
    let content = match collect(&ids.model, deltas).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    Json(ids.response_object(
        "completed",
        Some(&content),
        Some(Usage::estimate(&prompt, &content)),
    ))
    .into_response()
}

struct ResponseIds {
    response: String,
    message: String,
    created: u64,
    model: String,
}

impl ResponseIds {
    fn message_item(&self, status: &str, text: Option<&str>) -> serde_json::Value {
        let content: Vec<serde_json::Value> = text.map(|t| vec![Self::part(t)]).unwrap_or_default();
        json!({ "id": self.message, "type": "message", "status": status, "role": "assistant", "content": content })
    }

    fn part(text: &str) -> serde_json::Value {
        json!({ "type": "output_text", "text": text, "annotations": [] })
    }

    fn response_object(
        &self,
        status: &str,
        text: Option<&str>,
        usage: Option<Usage>,
    ) -> serde_json::Value {
        let output: Vec<serde_json::Value> = text
            .map(|t| vec![self.message_item("completed", Some(t))])
            .unwrap_or_default();
        let usage = usage.map(|u| json!({ "input_tokens": u.prompt_tokens, "output_tokens": u.completion_tokens, "total_tokens": u.total_tokens }));
        json!({
            "id": self.response, "object": "response", "created_at": self.created, "status": status,
            "model": self.model, "output": output, "usage": usage, "error": null, "incomplete_details": null,
        })
    }
}

fn responses_sse(ids: ResponseIds, prompt: String, mut deltas: DeltaStream) -> Response {
    let events = stream! {
        let mut seq = 0u64;
        let mut ev = |kind: &str, mut data: serde_json::Value| {
            data["type"] = json!(kind);
            data["sequence_number"] = json!(seq);
            seq += 1;
            Ok::<_, Infallible>(Event::default().event(kind).json_data(data).expect("serializable"))
        };
        let (item_id, model) = (ids.message.clone(), ids.model.clone());
        yield ev("response.created", json!({ "response": ids.response_object("in_progress", None, None) }));
        yield ev("response.in_progress", json!({ "response": ids.response_object("in_progress", None, None) }));
        yield ev("response.output_item.added", json!({ "output_index": 0, "item": ids.message_item("in_progress", Some("")) }));
        yield ev("response.content_part.added", json!({ "item_id": item_id, "output_index": 0, "content_index": 0, "part": ResponseIds::part("") }));
        let mut text = String::new();
        while let Some(item) = deltas.next().await {
            match item {
                Ok(delta) => {
                    text.push_str(&delta);
                    yield ev("response.output_text.delta", json!({ "item_id": item_id, "output_index": 0, "content_index": 0, "delta": delta }));
                }
                Err(e) => {
                    tracing::warn!(model, "{e}");
                    let mut failed = ids.response_object("failed", None, None);
                    failed["error"] = json!({ "code": "provider_error", "message": e.to_string() });
                    yield ev("error", json!({ "code": "provider_error", "message": e.to_string(), "param": null }));
                    yield ev("response.failed", json!({ "response": failed }));
                    return;
                }
            }
        }
        yield ev("response.output_text.done", json!({ "item_id": item_id, "output_index": 0, "content_index": 0, "text": text }));
        yield ev("response.content_part.done", json!({ "item_id": item_id, "output_index": 0, "content_index": 0, "part": ResponseIds::part(&text) }));
        yield ev("response.output_item.done", json!({ "output_index": 0, "item": ids.message_item("completed", Some(&text)) }));
        let usage = Usage::estimate(&prompt, &text);
        yield ev("response.completed", json!({ "response": ids.response_object("completed", Some(&text), Some(usage)) }));
    };
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse(id: String, created: u64, model: String, mut deltas: DeltaStream) -> Response {
    let events = stream! {
        let first = ChatChunk::new(&id, created, &model, Delta { role: Some("assistant"), content: Some(String::new()) }, None);
        yield Ok::<_, Infallible>(Event::default().json_data(first).expect("serializable"));
        while let Some(item) = deltas.next().await {
            match item {
                Ok(text) => {
                    let chunk = ChatChunk::new(&id, created, &model, Delta { role: None, content: Some(text) }, None);
                    yield Ok(Event::default().json_data(chunk).expect("serializable"));
                }
                Err(e) => {
                    tracing::warn!(model, "{e}");
                    let body = ErrorBody { error: ApiError { message: e.to_string(), kind: "server_error", param: None, code: Some("provider_error") } };
                    yield Ok(Event::default().json_data(body).expect("serializable"));
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            }
        }
        let last = ChatChunk::new(&id, created, &model, Delta::default(), Some("stop"));
        yield Ok(Event::default().json_data(last).expect("serializable"));
        yield Ok(Event::default().data("[DONE]"));
    };
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}
