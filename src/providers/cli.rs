//! Claude Code and Junie: one process per request, NDJSON on stdout.

use std::sync::RwLock;
use std::time::{Duration, Instant};

use async_stream::try_stream;
use serde_json::Value;

use super::process::{self, Launch};
use super::{DeltaStream, ProviderError};
use crate::config::{AgentConfig, ProviderConfig};
use crate::openai::ChatInput;

/// Claude Code has no list command; these are the current API ids (aliases come from `--help`).
const CLAUDE_MODELS: &[&str] = &[
    "claude-fable-5-1",
    "claude-fable-5",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-5-5",
    "claude-sonnet-5",
    "claude-sonnet-4-6",
    "claude-haiku-5-5",
    "claude-haiku-4-5",
];

#[derive(Clone, Copy, Debug)]
pub enum Flavor {
    /// `claude -p --output-format stream-json --include-partial-messages`, prompt on stdin.
    Claude,
    /// `junie --output-format=json-stream --task=<prompt>`.
    Junie,
}

pub struct CliProvider {
    pub cfg: ProviderConfig,
    pub flavor: Flavor,
    /// Models reported by the CLI (`discover`), exposed next to the configured agents.
    discovered: RwLock<Vec<String>>,
}

impl CliProvider {
    pub fn new(cfg: ProviderConfig, flavor: Flavor) -> Self {
        Self {
            cfg,
            flavor,
            discovered: RwLock::new(Vec::new()),
        }
    }

    pub fn discovered(&self) -> Vec<String> {
        self.discovered.read().unwrap().clone()
    }

    /// Asks the CLI for its models and stores them. Junie prints the list when given an
    /// unknown `--model`; Claude's aliases are parsed from `--help`, full ids are built in.
    pub async fn discover(&self) -> Result<Vec<String>, ProviderError> {
        let mut launch = Launch::new(&self.cfg);
        let mut models: Vec<String> = Vec::new();
        match self.flavor {
            Flavor::Junie => {
                launch.args([
                    "--skip-update-check",
                    "--model=__list__",
                    "--task=list models",
                ]);
                let out = self.output(launch).await?;
                models.extend(
                    out.lines()
                        .filter_map(|l| l.strip_prefix("- "))
                        .map(|m| m.trim().to_string()),
                );
            }
            Flavor::Claude => {
                launch.arg("--help");
                let out = self.output(launch).await?;
                let block = out.split("--model <model>").nth(1).unwrap_or("");
                let block = block.split("\n  -").next().unwrap_or("");
                for part in block.split('\'').skip(1).step_by(2) {
                    if !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase()) {
                        models.push(part.to_string());
                    }
                }
                models.extend(CLAUDE_MODELS.iter().map(|m| m.to_string()));
            }
        }
        models.retain(|m| !m.is_empty());
        models.dedup();
        tracing::info!(provider = %self.cfg.id, count = models.len(), "models discovered");
        *self.discovered.write().unwrap() = models.clone();
        Ok(models)
    }

    /// stdout + stderr of a short run (Junie answers on stderr).
    async fn output(&self, launch: Launch) -> Result<String, ProviderError> {
        let mut cmd = launch.command().await;
        let id = &self.cfg.id;
        let out = tokio::time::timeout(Duration::from_secs(120), cmd.output())
            .await
            .map_err(|_| ProviderError(format!("{id}: model discovery timed out")))?
            .map_err(|e| {
                ProviderError(format!("{id}: cannot start {}: {e}", self.cfg.command()))
            })?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(text)
    }
    fn build(&self, agent: &AgentConfig, input: &ChatInput) -> (Launch, Option<String>) {
        let mut cmd = Launch::new(&self.cfg);
        let system: Vec<&str> = [
            agent.system_prompt.as_str(),
            input.system.as_deref().unwrap_or(""),
        ]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();
        let system = (!system.is_empty()).then(|| system.join("\n\n"));
        match self.flavor {
            Flavor::Claude => {
                cmd.args([
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                ]);
                if !agent.model.is_empty() {
                    cmd.args(["--model", &agent.model]);
                }
                if let Some(s) = &system {
                    cmd.args(["--append-system-prompt", s]);
                }
                cmd.args(&self.cfg.extra_args).args(&agent.extra_args);
                (cmd, Some(input.prompt.clone()))
            }
            Flavor::Junie => {
                cmd.args(["--skip-update-check", "--output-format=json-stream"]);
                if let Some(d) = cmd.cwd.clone() {
                    cmd.arg(format!("--project={d}"));
                }
                if !agent.model.is_empty() {
                    cmd.arg(format!("--model={}", agent.model));
                }
                if let Some(s) = &system {
                    cmd.arg(format!("--system-prompt={s}"));
                }
                cmd.args(&self.cfg.extra_args).args(&agent.extra_args);
                cmd.arg(format!("--task={}", input.prompt));
                (cmd, None)
            }
        }
    }

    pub fn chat(&self, agent: &AgentConfig, input: ChatInput) -> DeltaStream {
        let (launch, stdin) = self.build(agent, &input);
        let timeout = Duration::from_secs(self.cfg.timeout_secs.max(1));
        let flavor = self.flavor;
        let id = self.cfg.id.clone();
        Box::pin(try_stream! {
            let cmd = launch.command().await;
            let mut run = process::spawn(cmd, stdin).await?;
            tracing::debug!(provider = %id, program = %run.program, "started");
            let deadline = tokio::time::Instant::from_std(Instant::now() + timeout);
            let mut parser = Parser { flavor, streamed: false, result: None };
            let mut yielded = false;
            loop {
                let next = tokio::select! {
                    l = run.lines.next_line() => l.map_err(|e| ProviderError(format!("{id}: {e}"))),
                    _ = tokio::time::sleep_until(deadline) => {
                        Err(ProviderError(format!("{id}: no answer within {}s", timeout.as_secs())))
                    }
                };
                let line = next?;
                let Some(line) = line else { break };
                match parser.line(&line) {
                    Event::Delta(text) => {
                        yielded = true;
                        yield text;
                    }
                    Event::Error(message) => Err(ProviderError(format!("{id}: {message}")))?,
                    Event::None => {}
                }
            }
            let status = run.child.0.wait().await.map_err(|e| ProviderError(format!("{id}: {e}")))?;
            if !status.success() && !yielded && parser.result.is_none() {
                Err(ProviderError(format!("{id}: {} exited with {status}: {}", run.program, run.stderr_tail())))?;
            }
            if let Some(text) = parser.result.take() {
                if !yielded {
                    yield text;
                }
            }
        })
    }
}

enum Event {
    Delta(String),
    Error(String),
    None,
}

struct Parser {
    flavor: Flavor,
    /// Claude sent partial deltas, so the final `result` must not be repeated.
    streamed: bool,
    /// Full answer for CLIs (or versions) that only report it at the end.
    result: Option<String>,
}

impl Parser {
    fn line(&mut self, line: &str) -> Event {
        let line = line.trim();
        if !line.starts_with('{') {
            if !line.is_empty() {
                tracing::debug!(target: "stdout", "{line}");
            }
            return Event::None;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Event::None;
        };
        match self.flavor {
            Flavor::Claude => self.claude(&v),
            Flavor::Junie => self.junie(&v),
        }
    }

    fn claude(&mut self, v: &Value) -> Event {
        match v["type"].as_str().unwrap_or("") {
            "stream_event" => {
                let ev = &v["event"];
                if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                    if let Some(t) = ev["delta"]["text"].as_str() {
                        self.streamed = true;
                        return Event::Delta(t.to_string());
                    }
                }
                Event::None
            }
            "assistant" if !self.streamed => {
                if let Some(blocks) = v["message"]["content"].as_array() {
                    let text: String = blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect();
                    if !text.is_empty() {
                        self.result = Some(text);
                    }
                }
                Event::None
            }
            "result" => {
                if v["is_error"].as_bool().unwrap_or(false) {
                    let msg = v["result"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v["errors"].to_string());
                    return Event::Error(msg);
                }
                if !self.streamed {
                    if let Some(r) = v["result"].as_str() {
                        self.result = Some(r.to_string());
                    }
                }
                Event::None
            }
            _ => Event::None,
        }
    }

    fn junie(&mut self, v: &Value) -> Event {
        match v["type"].as_str().unwrap_or("") {
            "step" => {
                if let Some(name) = v["name"].as_str() {
                    tracing::info!(target: "junie", "step: {name}");
                }
                Event::None
            }
            "result" => {
                if let Some(r) = v["result"].as_str() {
                    self.result = Some(r.to_string());
                }
                Event::None
            }
            "error" => Event::Error(
                v["message"]
                    .as_str()
                    .or(v["error"].as_str())
                    .unwrap_or("junie reported an error")
                    .to_string(),
            ),
            _ => Event::None,
        }
    }
}
