//! The subset of the OpenAI chat API this proxy speaks.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: Content,
}

/// `content` is a string or an array of parts; only text parts are used.
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
    #[default]
    Empty,
}

#[derive(Debug, Deserialize)]
pub struct Part {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
}

impl Content {
    pub fn text(&self) -> String {
        match self {
            Content::Text(t) => t.clone(),
            Content::Parts(parts) => parts
                .iter()
                .filter(|p| p.kind == "text")
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            Content::Empty => String::new(),
        }
    }
}

/// What a provider gets: one prompt, plus the system messages separately.
#[derive(Debug, Clone)]
pub struct ChatInput {
    pub prompt: String,
    pub system: Option<String>,
}

/// A lone user message is passed as is; a conversation is flattened with `[role]` headers.
pub fn build_input(messages: &[Message]) -> ChatInput {
    let mut system = Vec::new();
    let mut turns: Vec<(&str, String)> = Vec::new();
    for m in messages {
        let text = m.content.text();
        if text.trim().is_empty() {
            continue;
        }
        match m.role.as_str() {
            "system" | "developer" => system.push(text),
            role => turns.push((role, text)),
        }
    }
    let prompt = match turns.as_slice() {
        [(role, text)] if *role == "user" => text.clone(),
        _ => {
            let mut s = turns
                .iter()
                .map(|(r, t)| format!("[{r}]\n{t}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            s.push_str("\n\n[assistant]\n");
            s
        }
    };
    ChatInput {
        prompt,
        system: (!system.is_empty()).then(|| system.join("\n\n")),
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
}

/// Rough token estimate; the agents do not report usage.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.chars().count() / 4) as u32
}

#[derive(Serialize)]
pub struct ModelList {
    pub object: &'static str,
    pub data: Vec<Model>,
}

#[derive(Serialize)]
pub struct Model {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub owned_by: String,
}

impl Model {
    pub fn new(id: &str) -> Self {
        let owner = id.split('/').next().unwrap_or("aiproxy").to_string();
        Self {
            id: id.to_string(),
            object: "model",
            created: 0,
            owned_by: owner,
        }
    }
}

#[derive(Serialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

#[derive(Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: &'static str,
}

#[derive(Serialize)]
pub struct AssistantMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Serialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl Usage {
    pub fn estimate(prompt: &str, completion: &str) -> Self {
        let p = estimate_tokens(prompt);
        let c = estimate_tokens(completion);
        Self {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: p + c,
        }
    }
}

#[derive(Serialize)]
pub struct ChatChunk {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

#[derive(Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<&'static str>,
}

#[derive(Serialize, Default)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl ChatChunk {
    pub fn new(
        id: &str,
        created: u64,
        model: &str,
        delta: Delta,
        finish: Option<&'static str>,
    ) -> Self {
        Self {
            id: id.to_string(),
            object: "chat.completion.chunk",
            created,
            model: model.to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta,
                finish_reason: finish,
            }],
        }
    }
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub error: ApiError,
}

#[derive(Serialize)]
pub struct ApiError {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub param: Option<String>,
    pub code: Option<&'static str>,
}
