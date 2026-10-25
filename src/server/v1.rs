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
use crate::config::ToolMode;
use crate::openai::*;
use crate::providers::{DeltaStream, ProviderError};
use crate::tools;

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

/// A started chat: the resolved model id, the prompt (for the usage estimate), the text deltas and,
/// with tool emulation, the parsed tool calls (the deltas are then the text before the call block).
struct Started {
    model: String,
    prompt: String,
    deltas: DeltaStream,
    tool_calls: Vec<ToolCall>,
    /// Emulation found only tool calls: no assistant text at all.
    no_text: bool,
}

impl Started {
    fn finish(&self) -> &'static str {
        if self.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        }
    }
}

/// Resolves the model and waits for the first delta so that a provider failure is a real HTTP error.
async fn start(
    state: &AppState,
    model: &str,
    messages: &[Message],
    tools: &[ToolSpec],
    stream: bool,
) -> Result<Started, Box<Response>> {
    let registry = state.registry.read().unwrap().clone();
    let Some(resolved) = registry.resolve(model) else {
        return Err(Box::new(not_found(model)));
    };
    if let Some(kind) = messages.iter().find_map(|m| m.content.unsupported()) {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            format!("content part type {kind:?} is not supported: text and images only"),
            "invalid_request_error",
            Some("unsupported_content"),
        )));
    }
    let emulate = resolved.agent.tools == ToolMode::Emulate && !tools.is_empty();
    let mut input = match build_input(messages) {
        Ok(i) => i,
        Err(e) => {
            return Err(Box::new(error(
                StatusCode::BAD_REQUEST,
                e,
                "invalid_request_error",
                Some("unsupported_content"),
            )))
        }
    };
    if !input.images.is_empty() && !resolved.provider.cfg().images {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            format!(
                "provider {:?} takes text only (images = false in the aiproxy settings)",
                resolved.provider.cfg().id
            ),
            "invalid_request_error",
            Some("unsupported_content"),
        )));
    }
    let limit = resolved.provider.cfg().max_prompt_chars;
    let chars =
        input.prompt.chars().count() + input.system.as_deref().map_or(0, |s| s.chars().count());
    if limit > 0 && chars > limit {
        tracing::warn!(model = %resolved.model_id, chars, limit, "prompt too long");
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            format!("prompt is {chars} characters; provider {:?} accepts up to {limit} (max_prompt_chars in the aiproxy settings)", resolved.provider.cfg().id),
            "invalid_request_error",
            Some("context_length_exceeded"),
        )));
    }
    if emulate {
        let block = tools::prompt(tools);
        input.system = Some(match input.system {
            Some(s) => format!("{s}\n\n{block}"),
            None => block,
        });
    }
    tracing::info!(model = %resolved.model_id, prompt_chars = input.prompt.len(), images = input.images.len(), stream, tools = tools.len(), emulate, "chat");
    let prompt = input.prompt.clone();
    let model = resolved.model_id.clone();
    let mut deltas = resolved.chat(input);
    let first = match deltas.next().await {
        Some(Err(e)) => {
            tracing::warn!(model, "{e}");
            return Err(Box::new(upstream(&e)));
        }
        Some(Ok(t)) => Some(t),
        None => None,
    };
    let deltas: DeltaStream = Box::pin(futures::stream::iter(first.map(Ok)).chain(deltas));
    if !emulate {
        return Ok(Started {
            model,
            prompt,
            deltas,
            tool_calls: Vec::new(),
            no_text: false,
        });
    }
    let text = collect(&model, deltas).await?;
    let (text, tool_calls) = tools::parse(&text).unwrap_or((text, Vec::new()));
    if !tool_calls.is_empty() {
        let names: Vec<&str> = tool_calls.iter().map(|c| c.name.as_str()).collect();
        tracing::info!(model, ?names, "tool calls emulated");
    }
    let no_text = text.is_empty() && !tool_calls.is_empty();
    let items = (!text.is_empty()).then_some(Ok(text));
    Ok(Started {
        model,
        prompt,
        deltas: Box::pin(futures::stream::iter(items)),
        tool_calls,
        no_text,
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
    let tools = tool_specs(&req.tools);
    let started = match start(&state, &req.model, &req.messages, &tools, req.stream).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let id = new_id();
    let created = now();
    if req.stream {
        return sse(id, created, started);
    }
    let finish = started.finish();
    let Started {
        model,
        prompt,
        deltas,
        tool_calls,
        no_text,
    } = started;
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
                content: (!no_text).then_some(content),
                tool_calls: (!tool_calls.is_empty()).then(|| {
                    tool_calls
                        .iter()
                        .map(|c| ToolCallOut::new(c, None))
                        .collect()
                }),
            },
            finish_reason: finish,
        }],
        usage,
    };
    Json(response).into_response()
}

fn sse(id: String, created: u64, started: Started) -> Response {
    let finish = started.finish();
    let Started {
        model,
        mut deltas,
        tool_calls,
        ..
    } = started;
    let events = stream! {
        let first = ChatChunk::new(&id, created, &model, Delta { role: Some("assistant"), content: Some(String::new()), tool_calls: None }, None);
        yield Ok::<_, Infallible>(Event::default().json_data(first).expect("serializable"));
        while let Some(item) = deltas.next().await {
            match item {
                Ok(text) => {
                    let chunk = ChatChunk::new(&id, created, &model, Delta { role: None, content: Some(text), tool_calls: None }, None);
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
        if !tool_calls.is_empty() {
            let calls = tool_calls.iter().enumerate().map(|(i, c)| ToolCallOut::new(c, Some(i as u32))).collect();
            let chunk = ChatChunk::new(&id, created, &model, Delta { role: None, content: None, tool_calls: Some(calls) }, None);
            yield Ok(Event::default().json_data(chunk).expect("serializable"));
        }
        let last = ChatChunk::new(&id, created, &model, Delta::default(), Some(finish));
        yield Ok(Event::default().json_data(last).expect("serializable"));
        yield Ok(Event::default().data("[DONE]"));
    };
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// `POST /v1/responses`: the Responses API with one `output_text` message and, with tool
/// emulation, `function_call` items.
pub async fn responses(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ResponsesRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return bad_request(e),
    };
    let messages = req.messages();
    let tools = tool_specs(&req.tools);
    let started = match start(&state, &req.model, &messages, &tools, req.stream).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let ids = ResponseIds {
        response: id("resp"),
        message: id("msg"),
        created: now(),
        model: started.model.clone(),
    };
    if req.stream {
        return responses_sse(ids, started);
    }
    let Started {
        model,
        prompt,
        deltas,
        tool_calls,
        no_text,
    } = started;
    let content = match collect(&model, deltas).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let mut output = Vec::new();
    if !no_text {
        output.push(ids.message_item("completed", Some(&content)));
    }
    output.extend(
        tool_calls
            .iter()
            .map(|c| ResponseIds::call_item(c, "completed")),
    );
    let usage = Usage::estimate(&prompt, &content);
    Json(ids.response_object("completed", output, Some(usage))).into_response()
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

    fn call_item(c: &ToolCall, status: &str) -> serde_json::Value {
        json!({ "id": format!("fc_{}", c.id), "type": "function_call", "status": status,
                "call_id": c.id, "name": c.name, "arguments": c.arguments })
    }

    fn response_object(
        &self,
        status: &str,
        output: Vec<serde_json::Value>,
        usage: Option<Usage>,
    ) -> serde_json::Value {
        let usage = usage.map(|u| json!({ "input_tokens": u.prompt_tokens, "output_tokens": u.completion_tokens, "total_tokens": u.total_tokens }));
        json!({
            "id": self.response, "object": "response", "created_at": self.created, "status": status,
            "model": self.model, "output": output, "usage": usage, "error": null, "incomplete_details": null,
        })
    }
}

fn responses_sse(ids: ResponseIds, started: Started) -> Response {
    let Started {
        model,
        prompt,
        mut deltas,
        tool_calls,
        no_text,
    } = started;
    let events = stream! {
        let mut seq = 0u64;
        let mut ev = |kind: &str, mut data: serde_json::Value| {
            data["type"] = json!(kind);
            data["sequence_number"] = json!(seq);
            seq += 1;
            Ok::<_, Infallible>(Event::default().event(kind).json_data(data).expect("serializable"))
        };
        let item_id = ids.message.clone();
        yield ev("response.created", json!({ "response": ids.response_object("in_progress", Vec::new(), None) }));
        yield ev("response.in_progress", json!({ "response": ids.response_object("in_progress", Vec::new(), None) }));
        let mut output = Vec::new();
        let mut text = String::new();
        if !no_text {
            yield ev("response.output_item.added", json!({ "output_index": 0, "item": ids.message_item("in_progress", Some("")) }));
            yield ev("response.content_part.added", json!({ "item_id": item_id, "output_index": 0, "content_index": 0, "part": ResponseIds::part("") }));
            while let Some(item) = deltas.next().await {
                match item {
                    Ok(delta) => {
                        text.push_str(&delta);
                        yield ev("response.output_text.delta", json!({ "item_id": item_id, "output_index": 0, "content_index": 0, "delta": delta }));
                    }
                    Err(e) => {
                        tracing::warn!(model, "{e}");
                        let mut failed = ids.response_object("failed", Vec::new(), None);
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
            output.push(ids.message_item("completed", Some(&text)));
        }
        for c in &tool_calls {
            let index = output.len();
            let mut added = ResponseIds::call_item(c, "in_progress");
            added["arguments"] = json!("");
            let fc_id = added["id"].clone();
            yield ev("response.output_item.added", json!({ "output_index": index, "item": added }));
            yield ev("response.function_call_arguments.delta", json!({ "item_id": fc_id, "output_index": index, "delta": c.arguments }));
            yield ev("response.function_call_arguments.done", json!({ "item_id": fc_id, "output_index": index, "arguments": c.arguments }));
            let done = ResponseIds::call_item(c, "completed");
            yield ev("response.output_item.done", json!({ "output_index": index, "item": done }));
            output.push(done);
        }
        let usage = Usage::estimate(&prompt, &text);
        yield ev("response.completed", json!({ "response": ids.response_object("completed", output, Some(usage)) }));
    };
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}
