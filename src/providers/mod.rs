//! Providers turn a chat into a stream of text deltas. Models are `<provider id>/<agent name>`.

pub mod cli;
pub mod deepseek;
pub mod process;

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;

use crate::config::{AgentConfig, Paths, ProviderConfig, ProviderKind, Settings};
use crate::openai::ChatInput;

#[derive(Debug, Clone)]
pub struct ProviderError(pub String);

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProviderError {}

pub type DeltaStream = Pin<Box<dyn Stream<Item = Result<String, ProviderError>> + Send>>;

#[allow(clippy::large_enum_variant)]
pub enum Provider {
    Cli(cli::CliProvider),
    Deepseek(Arc<deepseek::DeepseekProvider>),
}

impl Provider {
    pub fn cfg(&self) -> &ProviderConfig {
        match self {
            Provider::Cli(p) => &p.cfg,
            Provider::Deepseek(p) => &p.cfg,
        }
    }

    fn agents(&self) -> Vec<AgentConfig> {
        let cfg = self.cfg();
        if cfg.agents.is_empty() {
            return vec![AgentConfig {
                name: cfg.id.clone(),
                ..Default::default()
            }];
        }
        cfg.agents.clone()
    }

    /// Configured agents first, then the CLI's own models (pass-through agents).
    pub fn model_ids(&self) -> Vec<String> {
        let id = &self.cfg().id;
        let mut ids: Vec<String> = self
            .agents()
            .iter()
            .map(|a| format!("{id}/{}", a.name))
            .collect();
        for m in self.discovered() {
            let full = format!("{id}/{m}");
            if !ids.contains(&full) {
                ids.push(full);
            }
        }
        ids
    }

    pub fn discovered(&self) -> Vec<String> {
        match self {
            Provider::Cli(p) => p.discovered(),
            Provider::Deepseek(_) => Vec::new(),
        }
    }

    pub async fn discover(&self) -> Result<Vec<String>, ProviderError> {
        match self {
            Provider::Cli(p) if p.cfg.discover_models => p.discover().await,
            _ => Ok(Vec::new()),
        }
    }

    pub fn chat(&self, agent: &AgentConfig, input: ChatInput) -> DeltaStream {
        match self {
            Provider::Cli(p) => p.chat(agent, input),
            Provider::Deepseek(p) => p.chat(input),
        }
    }
}

pub struct Resolved {
    pub provider: Arc<Provider>,
    pub agent: AgentConfig,
    pub model_id: String,
}

impl Resolved {
    pub fn chat(&self, input: ChatInput) -> DeltaStream {
        self.provider.chat(&self.agent, input)
    }
}

pub struct Registry {
    providers: Vec<Arc<Provider>>,
}

impl Registry {
    pub fn build(settings: &Settings, paths: &Paths) -> Self {
        let providers = settings
            .providers
            .iter()
            .filter(|c| c.enabled)
            .map(|c| {
                let p = match c.kind {
                    ProviderKind::ClaudeCli => {
                        Provider::Cli(cli::CliProvider::new(c.clone(), cli::Flavor::Claude))
                    }
                    ProviderKind::JunieCli => {
                        Provider::Cli(cli::CliProvider::new(c.clone(), cli::Flavor::Junie))
                    }
                    ProviderKind::DeepseekWeb => Provider::Deepseek(Arc::new(
                        deepseek::DeepseekProvider::new(c.clone(), paths),
                    )),
                };
                Arc::new(p)
            })
            .collect();
        Self { providers }
    }

    pub fn provider(&self, id: &str) -> Option<Arc<Provider>> {
        self.providers.iter().find(|p| p.cfg().id == id).cloned()
    }

    /// Runs model discovery of every provider in the background (needs a Tokio context).
    pub fn start_discovery(&self) {
        for p in &self.providers {
            let p = p.clone();
            tokio::spawn(async move {
                if let Err(e) = p.discover().await {
                    tracing::warn!("{e}");
                }
            });
        }
    }

    pub fn models(&self) -> Vec<String> {
        self.providers.iter().flat_map(|p| p.model_ids()).collect()
    }

    /// `<id>/<agent>`; a bare `<id>` means its first agent. For CLI providers an
    /// unknown agent name is passed through as the CLI's `--model`.
    pub fn resolve(&self, model: &str) -> Option<Resolved> {
        let (pid, agent_name) = match model.split_once('/') {
            Some((p, a)) => (p, Some(a)),
            None => (model, None),
        };
        let provider = self.providers.iter().find(|p| p.cfg().id == pid)?.clone();
        let agents = provider.agents();
        let agent = match agent_name {
            None => agents.first().cloned()?,
            Some(name) => match agents.iter().find(|a| a.name == name) {
                Some(a) => a.clone(),
                None if matches!(*provider, Provider::Cli(_)) => AgentConfig {
                    name: name.into(),
                    model: name.into(),
                    ..Default::default()
                },
                None => return None,
            },
        };
        Some(Resolved {
            model_id: format!("{pid}/{}", agent.name),
            provider,
            agent,
        })
    }

    pub fn deepseek(&self, id: &str) -> Option<Arc<deepseek::DeepseekProvider>> {
        self.providers.iter().find_map(|p| match p.as_ref() {
            Provider::Deepseek(d) if d.cfg.id == id => Some(d.clone()),
            _ => None,
        })
    }
}
