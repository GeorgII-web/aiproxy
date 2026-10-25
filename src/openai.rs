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
    #[serde(default)]
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: Content,
    /// Earlier tool calls of the assistant (chat completions history).
    #[serde(default)]
    pub tool_calls: Vec<ToolCallMsg>,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallMsg {
    #[serde(default)]
    pub function: FunctionCallMsg,
}

#[derive(Debug, Default, Deserialize)]
pub struct FunctionCallMsg {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

/// A tool definition in either shape: chat completions (`{type, function: {name, ...}}`) or
/// Responses (`{type, name, ...}`).
#[derive(Debug, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub function: Option<ToolSpec>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Only function tools; others (web search, file search ...) are dropped.
pub fn tool_specs(defs: &[ToolDef]) -> Vec<ToolSpec> {
    defs.iter()
        .filter_map(|d| match (&d.function, &d.name) {
            (Some(f), _) => Some(f.clone()),
            (None, Some(name)) if d.kind == "function" || d.kind.is_empty() => Some(ToolSpec {
                name: name.clone(),
                description: d.description.clone().unwrap_or_default(),
                parameters: d.parameters.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// A tool call the model asked for; `arguments` is a JSON object as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub fn tool_call_text(name: &str, arguments: &str) -> String {
    format!("[tool call] {name}({arguments})")
}

/// `content` is a string or an array of parts (text and images).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
    #[default]
    Empty,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Part {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    /// `image_url` part: `{"url": "data:..."}` (chat completions) or the URL itself (Responses `input_image`).
    #[serde(default)]
    pub image_url: Option<ImageUrl>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImageUrl {
    Url(String),
    Object { url: String },
}

impl ImageUrl {
    pub fn url(&self) -> &str {
        match self {
            ImageUrl::Url(u) | ImageUrl::Object { url: u } => u,
        }
    }
}

/// An image attached to a message, decoded from its `data:` URL.
#[derive(Debug, Clone)]
pub struct Image {
    /// `png`, `jpeg`, `gif`, `webp`.
    pub format: String,
    pub bytes: Vec<u8>,
}

impl Image {
    /// `data:image/png;base64,...` → the bytes; anything else is unsupported.
    pub fn from_data_url(url: &str) -> Result<Self, String> {
        use base64::Engine;
        let Some(rest) = url.strip_prefix("data:image/") else {
            return Err("only data: URLs with a base64 image are supported".into());
        };
        let (format, data) = rest
            .split_once(";base64,")
            .ok_or("only data: URLs with a base64 image are supported")?;
        let format = match format {
            "jpg" => "jpeg",
            f => f,
        };
        if !matches!(format, "png" | "jpeg" | "gif" | "webp") {
            return Err(format!("image format {format:?} is not supported"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| format!("invalid base64 image: {e}"))?;
        Ok(Self {
            format: format.into(),
            bytes,
        })
    }
}

impl Content {
    /// Part types the proxy cannot pass on (audio, files).
    pub fn unsupported(&self) -> Option<&str> {
        match self {
            Content::Parts(parts) => parts.iter().map(|p| p.kind.as_str()).find(|k| {
                !matches!(
                    *k,
                    "text" | "input_text" | "output_text" | "refusal" | "image_url" | "input_image"
                )
            }),
            _ => None,
        }
    }

    /// The images of the message, in order.
    pub fn images(&self) -> Result<Vec<Image>, String> {
        match self {
            Content::Parts(parts) => parts
                .iter()
                .filter(|p| matches!(p.kind.as_str(), "image_url" | "input_image"))
                .map(|p| match &p.image_url {
                    Some(u) => Image::from_data_url(u.url()),
                    None => Err("image part without image_url".into()),
                })
                .collect(),
            _ => Ok(Vec::new()),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Content::Text(t) => t.clone(),
            Content::Parts(parts) => parts
                .iter()
                .filter(|p| matches!(p.kind.as_str(), "text" | "input_text" | "output_text"))
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            Content::Empty => String::new(),
        }
    }
}

/// What a provider gets: one prompt, the system messages separately, and the attached images
/// (referenced in the prompt as `[image N]`, 1-based, in order).
#[derive(Debug, Clone, Default)]
pub struct ChatInput {
    pub prompt: String,
    pub system: Option<String>,
    pub images: Vec<Image>,
}

/// A lone user message is passed as is; a conversation is flattened with `[role]` headers.
/// Images that do not decode are reported by their message's number and reason.
pub fn build_input(messages: &[Message]) -> Result<ChatInput, String> {
    let mut system = Vec::new();
    let mut turns: Vec<(&str, String)> = Vec::new();
    let mut images = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let mut text = m.content.text();
        for img in m
            .content
            .images()
            .map_err(|e| format!("message {i}: {e}"))?
        {
            images.push(img);
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("[image {}]", images.len()));
        }
        for c in &m.tool_calls {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&tool_call_text(&c.function.name, &c.function.arguments));
        }
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
    Ok(ChatInput {
        prompt,
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        images,
    })
}

/// `POST /v1/responses` (OpenAI Responses API): `input` is a string or a list of items.
#[derive(Debug, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub input: ResponsesInput,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInput {
    Text(String),
    Items(Vec<InputItem>),
    #[default]
    Empty,
}

/// A message (`role` + `content`), a `function_call` or a `function_call_output`; other items are ignored.
#[derive(Debug, Deserialize)]
pub struct InputItem {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Content,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
}

impl ResponsesRequest {
    /// The same flattening as chat completions: `instructions` is the first system message.
    pub fn messages(&self) -> Vec<Message> {
        let mut messages = Vec::new();
        if let Some(i) = &self.instructions {
            messages.push(Message {
                role: "system".into(),
                content: Content::Text(i.clone()),
                tool_calls: Vec::new(),
            });
        }
        match &self.input {
            ResponsesInput::Text(t) => messages.push(Message {
                role: "user".into(),
                content: Content::Text(t.clone()),
                tool_calls: Vec::new(),
            }),
            ResponsesInput::Items(items) => {
                for it in items {
                    let (role, text) = match it.kind.as_deref() {
                        Some("function_call") => (
                            "assistant",
                            tool_call_text(
                                it.name.as_deref().unwrap_or("?"),
                                it.arguments.as_deref().unwrap_or(""),
                            ),
                        ),
                        Some("function_call_output") => {
                            ("tool", it.output.clone().unwrap_or_default())
                        }
                        None | Some("message") => {
                            messages.push(Message {
                                role: it.role.clone().unwrap_or_else(|| "user".into()),
                                content: it.content.clone(),
                                tool_calls: Vec::new(),
                            });
                            continue;
                        }
                        _ => continue,
                    };
                    messages.push(Message {
                        role: role.into(),
                        content: Content::Text(text),
                        tool_calls: Vec::new(),
                    });
                }
            }
            ResponsesInput::Empty => {}
        }
        messages
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_id() -> String {
    id("chatcmpl")
}

pub fn id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
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
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallOut>>,
}

/// Chat completions shape of a tool call (`index` only in stream chunks).
#[derive(Serialize)]
pub struct ToolCallOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionOut,
}

#[derive(Serialize)]
pub struct FunctionOut {
    pub name: String,
    pub arguments: String,
}

impl ToolCallOut {
    pub fn new(c: &ToolCall, index: Option<u32>) -> Self {
        Self {
            index,
            id: c.id.clone(),
            kind: "function",
            function: FunctionOut {
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            },
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallOut>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_in_messages() {
        let json = r#"[{"role":"user","content":[{"type":"text","text":"what is this"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,aGk="}},
            {"type":"input_image","image_url":"data:image/jpg;base64,aGk="}]}]"#;
        let messages: Vec<Message> = serde_json::from_str(json).unwrap();
        assert_eq!(messages[0].content.unsupported(), None);
        let input = build_input(&messages).unwrap();
        assert_eq!(input.prompt, "what is this\n[image 1]\n[image 2]");
        assert_eq!(input.images.len(), 2);
        assert_eq!(input.images[1].format, "jpeg");
        assert_eq!(input.images[0].bytes, b"hi");
        assert!(Image::from_data_url("https://x/y.png").is_err());
        assert!(Image::from_data_url("data:image/bmp;base64,aGk=").is_err());
    }
}
