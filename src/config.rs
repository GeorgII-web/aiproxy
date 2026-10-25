//! Settings file (`config.toml`) and the app's directories.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "AIProxy";

/// Where the app keeps its files: `%APPDATA%\aiproxy` on Windows,
/// `~/.config/aiproxy` elsewhere, or `$AIPROXY_HOME`.
#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub config_file: PathBuf,
    pub logs_dir: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let home = match std::env::var_os("AIPROXY_HOME") {
            Some(h) => PathBuf::from(h),
            None => directories::BaseDirs::new()
                .context("no home directory")?
                .config_dir()
                .join("aiproxy"),
        };
        let logs_dir = home.join("logs");
        std::fs::create_dir_all(&logs_dir)
            .with_context(|| format!("cannot create {}", logs_dir.display()))?;
        Ok(Self {
            config_file: home.join("config.toml"),
            logs_dir,
            home,
        })
    }

    /// `sidecar/` next to the executable, or the source tree's copy in dev.
    pub fn default_sidecar_dir() -> PathBuf {
        let next_to_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("sidecar")));
        match next_to_exe {
            Some(d) if d.join("deepseek.mjs").exists() => d,
            _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sidecar"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub server: ServerSettings,
    pub app: AppSettings,
    pub logging: LoggingSettings,
    pub providers: Vec<ProviderConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    /// Bind address. Keep loopback unless you set an api_key.
    pub host: String,
    pub port: u16,
    /// Empty = no authentication. Otherwise clients send `Authorization: Bearer <api_key>`.
    pub api_key: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Start with Windows (login item).
    pub autostart: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingSettings {
    /// trace | debug | info | warn | error, or a tracing filter string.
    pub level: String,
    /// Daily log files to keep.
    pub keep_files: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    ClaudeCli,
    JunieCli,
    DeepseekWeb,
}

impl ProviderKind {
    pub fn default_command(self) -> &'static str {
        match self {
            ProviderKind::ClaudeCli => "claude",
            ProviderKind::JunieCli => "junie",
            ProviderKind::DeepseekWeb => "node",
        }
    }
}

/// Where a provider's command runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    /// On this machine.
    #[default]
    Native,
    /// Inside WSL through `wsl.exe` (Windows host). On a Linux host this is the same as native.
    Wsl,
}

/// One provider = one `[[providers]]` table. Models are exposed as `<id>/<agent name>`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: ProviderKind,
    pub enabled: bool,
    pub runtime: Runtime,
    /// wsl: distribution name (`wsl -l`); empty = the default one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub wsl_distro: String,
    /// CLI kinds: ask the CLI for its model list at startup and expose every model as `<id>/<model>`.
    pub discover_models: bool,
    /// CLI binary (claude-cli, junie-cli) or the node binary (deepseek-web). Empty = kind default.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// Working directory of the CLI; for Junie the project it works in and may modify.
    /// With runtime = wsl a Linux path (`/home/...`) or a Windows path (converted to `/mnt/c/...`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub workdir: String,
    /// Hard kill of one request.
    pub timeout_secs: u64,
    /// Arguments appended to every run of this provider.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    /// deepseek-web: chat URL.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// deepseek-web: browser profile (cookies) directory. Empty = `<home>/deepseek-profile`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub profile_dir: String,
    /// deepseek-web: directory with `deepseek.mjs` and its node_modules. Empty = `sidecar/` next to the exe.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sidecar_dir: String,
    /// deepseek-web: Playwright browser channel: `chromium` (default), `chrome`, `msedge`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub browser: String,
    /// deepseek-web: JSON with `token` / `cookie` of an existing session (FreeDeepseekAPI's
    /// `deepseek-auth.json`), imported once before the login window opens.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub auth_file: String,
    pub agents: Vec<AgentConfig>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: ProviderKind::default(),
            enabled: true,
            runtime: Runtime::default(),
            wsl_distro: String::new(),
            discover_models: true,
            command: String::new(),
            workdir: String::new(),
            timeout_secs: 600,
            extra_args: Vec::new(),
            url: String::new(),
            profile_dir: String::new(),
            sidecar_dir: String::new(),
            browser: String::new(),
            auth_file: String::new(),
            agents: Vec::new(),
        }
    }
}

impl ProviderConfig {
    pub fn command(&self) -> &str {
        if self.command.is_empty() {
            self.kind.default_command()
        } else {
            &self.command
        }
    }
}

/// One exposed model of a provider.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Second half of the model id: `<provider id>/<name>`.
    pub name: String,
    /// Model passed to the CLI (`--model`). Empty = the CLI's default.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Extra system prompt for this agent.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub system_prompt: String,
    /// Arguments appended for this agent only (e.g. `["--agent", "reviewer"]` for Claude Code).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server: ServerSettings::default(),
            app: AppSettings::default(),
            logging: LoggingSettings::default(),
            providers: default_providers(),
        }
    }
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8787,
            api_key: String::new(),
        }
    }
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: "info".into(),
            keep_files: 7,
        }
    }
}

fn agent(name: &str, model: &str) -> AgentConfig {
    AgentConfig {
        name: name.into(),
        model: model.into(),
        ..Default::default()
    }
}

fn default_providers() -> Vec<ProviderConfig> {
    vec![
        ProviderConfig {
            id: "claude".into(),
            kind: ProviderKind::ClaudeCli,
            timeout_secs: 600,
            agents: vec![
                agent("sonnet", "sonnet"),
                agent("opus", "opus"),
                agent("haiku", "haiku"),
            ],
            ..Default::default()
        },
        ProviderConfig {
            id: "junie".into(),
            kind: ProviderKind::JunieCli,
            timeout_secs: 900,
            agents: vec![agent("default", "")],
            ..Default::default()
        },
        ProviderConfig {
            id: "deepseek".into(),
            kind: ProviderKind::DeepseekWeb,
            timeout_secs: 300,
            url: "https://chat.deepseek.com".into(),
            agents: vec![agent("deepseek", "")],
            ..Default::default()
        },
    ]
}

/// Only the providers, for the settings page.
#[derive(Serialize, Deserialize)]
struct ProvidersDoc {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
}

impl Settings {
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        if paths.config_file.exists() {
            let text = std::fs::read_to_string(&paths.config_file)?;
            let settings: Settings = toml::from_str(&text)
                .with_context(|| format!("invalid {}", paths.config_file.display()))?;
            settings.validate()?;
            return Ok(settings);
        }
        let settings = Settings::default();
        settings.save(paths)?;
        Ok(settings)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&paths.config_file, text)
            .with_context(|| format!("cannot write {}", paths.config_file.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if self.server.host.trim().is_empty() {
            bail!("server.host is empty");
        }
        if self.server.port == 0 {
            bail!("server.port is 0");
        }
        let mut ids = std::collections::HashSet::new();
        for p in &self.providers {
            if p.id.trim().is_empty() || p.id.contains('/') {
                bail!("provider id {:?} is invalid", p.id);
            }
            if !ids.insert(&p.id) {
                bail!("duplicate provider id {:?}", p.id);
            }
            let mut names = std::collections::HashSet::new();
            for a in &p.agents {
                if a.name.trim().is_empty() || a.name.contains('/') {
                    bail!("provider {:?}: agent name {:?} is invalid", p.id, a.name);
                }
                if !names.insert(&a.name) {
                    bail!("provider {:?}: duplicate agent {:?}", p.id, a.name);
                }
            }
        }
        Ok(())
    }

    pub fn providers_toml(&self) -> String {
        toml::to_string_pretty(&ProvidersDoc {
            providers: self.providers.clone(),
        })
        .unwrap_or_default()
    }

    pub fn parse_providers_toml(text: &str) -> Result<Vec<ProviderConfig>> {
        let doc: ProvidersDoc = toml::from_str(text).context("invalid providers TOML")?;
        Ok(doc.providers)
    }
}
