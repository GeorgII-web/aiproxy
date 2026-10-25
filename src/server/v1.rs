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

pub async fn chat(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                e.body_text(),
                "invalid_request_error",
                None,
            )
        }
    };
    let registry = state.registry.read().unwrap().clone();
    let Some(resolved) = registry.resolve(&req.model) else {
        return not_found(&req.model);
    };
    let input = build_input(&req.messages);
    tracing::info!(model = %resolved.model_id, prompt_chars = input.prompt.len(), stream = req.stream, "chat");
    let prompt = input.prompt.clone();
    let mut deltas = resolved.chat(input);

    // Wait for the first delta so that a provider failure is a real HTTP error.
    let first = match deltas.next().await {
        Some(Err(e)) => {
            tracing::warn!(model = %resolved.model_id, "{e}");
            return upstream(&e);
        }
        Some(Ok(t)) => Some(t),
        None => None,
    };
    let deltas: DeltaStream = Box::pin(futures::stream::iter(first.map(Ok)).chain(deltas));

    let id = new_id();
    let created = now();
    let model = resolved.model_id.clone();
    if req.stream {
        return sse(id, created, model, deltas);
    }
    let mut content = String::new();
    let mut deltas = deltas;
    while let Some(item) = deltas.next().await {
        match item {
            Ok(t) => content.push_str(&t),
            Err(e) => {
                tracing::warn!(model, "{e}");
                return upstream(&e);
            }
        }
    }
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
