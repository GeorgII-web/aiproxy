//! DeepSeek web chat through a Playwright sidecar (`sidecar/deepseek.mjs`).
//!
//! One long-lived node process drives one browser; requests are serialized.
//! Protocol: JSON lines both ways, see the sidecar for the messages.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, Mutex, OwnedMutexGuard};
use tokio_stream::wrappers::ReceiverStream;

use super::process::{ChildGuard, Launch};
use super::{DeltaStream, ProviderError};
use crate::config::{Paths, ProviderConfig};
use crate::openai::ChatInput;

pub struct DeepseekProvider {
    pub cfg: ProviderConfig,
    profile_dir: PathBuf,
    sidecar_dir: PathBuf,
    sidecar: Arc<Mutex<Option<Sidecar>>>,
}

#[derive(Deserialize)]
struct Msg {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    level: String,
}

struct Sidecar {
    _child: ChildGuard,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Sidecar {
    async fn start(p: &DeepseekProvider) -> Result<Self, ProviderError> {
        let mut launch = Launch::new(&p.cfg);
        let linux_paths = launch.is_wsl() && p.sidecar_dir.to_string_lossy().starts_with('/');
        if !linux_paths {
            // Linux paths of a WSL sidecar cannot be checked from Windows; the sidecar reports them.
            let script = p.sidecar_dir.join("deepseek.mjs");
            if !script.is_file() {
                return Err(ProviderError(format!(
                    "{}: sidecar not found: {}",
                    p.cfg.id,
                    script.display()
                )));
            }
            if !p.sidecar_dir.join("node_modules").is_dir() {
                return Err(ProviderError(format!(
                    "{}: run `npm install` in {} first",
                    p.cfg.id,
                    p.sidecar_dir.display()
                )));
            }
        }
        let dir = launch.path(&p.sidecar_dir);
        launch.cwd = Some(dir.clone());
        launch
            .arg(format!(
                "{}/deepseek.mjs",
                dir.trim_end_matches(['/', '\\'])
            ))
            .env(
                "AIPROXY_URL",
                if p.cfg.url.is_empty() {
                    "https://chat.deepseek.com"
                } else {
                    &p.cfg.url
                },
            )
            .env(
                "AIPROXY_CHANNEL",
                if p.cfg.browser.is_empty() {
                    "chromium"
                } else {
                    &p.cfg.browser
                },
            );
        if !p.cfg.auth_file.is_empty() {
            launch.env(
                "AIPROXY_AUTH_FILE",
                launch.path(std::path::Path::new(&p.cfg.auth_file)),
            );
        }
        // WSL default profile: the sidecar's own `<sidecar_dir>/profile`, a Linux path.
        if !(launch.is_wsl() && p.cfg.profile_dir.is_empty()) {
            launch.env("AIPROXY_PROFILE_DIR", launch.path(&p.profile_dir));
        }
        let (mut cmd, _temp) = launch.command().await?;
        cmd.stdin(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            ProviderError(format!(
                "{}: cannot start {}: {e}",
                p.cfg.id,
                p.cfg.command()
            ))
        })?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "deepseek", "{line}");
            }
        });
        let mut sc = Sidecar {
            _child: ChildGuard(child),
            stdin,
            lines: BufReader::new(stdout).lines(),
        };
        let ready = tokio::time::timeout(Duration::from_secs(60), sc.next()).await;
        match ready {
            Ok(Ok(m)) if m.kind == "ready" => Ok(sc),
            Ok(Ok(m)) => Err(ProviderError(format!(
                "deepseek sidecar: unexpected {} message",
                m.kind
            ))),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ProviderError(
                "deepseek sidecar did not become ready".into(),
            )),
        }
    }

    async fn send(&mut self, v: serde_json::Value) -> Result<(), ProviderError> {
        let mut line = v.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ProviderError(format!("deepseek sidecar: {e}")))
    }

    /// Next non-log message; log lines are forwarded to tracing on the way.
    async fn next(&mut self) -> Result<Msg, ProviderError> {
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .map_err(|e| ProviderError(format!("deepseek sidecar: {e}")))?
                .ok_or_else(|| ProviderError("deepseek sidecar exited".into()))?;
            let Ok(msg) = serde_json::from_str::<Msg>(&line) else {
                tracing::debug!(target: "deepseek", "{line}");
                continue;
            };
            if msg.kind == "log" {
                match msg.level.as_str() {
                    "error" => tracing::error!(target: "deepseek", "{}", msg.message),
                    "warn" => tracing::warn!(target: "deepseek", "{}", msg.message),
                    "debug" => tracing::debug!(target: "deepseek", "{}", msg.message),
                    _ => tracing::info!(target: "deepseek", "{}", msg.message),
                }
                continue;
            }
            return Ok(msg);
        }
    }
}

impl DeepseekProvider {
    pub fn new(cfg: ProviderConfig, paths: &Paths) -> Self {
        let profile_dir = if cfg.profile_dir.is_empty() {
            paths.home.join("deepseek-profile")
        } else {
            PathBuf::from(&cfg.profile_dir)
        };
        let sidecar_dir = if cfg.sidecar_dir.is_empty() {
            Paths::default_sidecar_dir()
        } else {
            PathBuf::from(&cfg.sidecar_dir)
        };
        Self {
            cfg,
            profile_dir,
            sidecar_dir,
            sidecar: Arc::new(Mutex::new(None)),
        }
    }

    async fn acquire(self: &Arc<Self>) -> Result<OwnedMutexGuard<Option<Sidecar>>, ProviderError> {
        let mut guard = self.sidecar.clone().lock_owned().await;
        if guard.is_none() {
            *guard = Some(Sidecar::start(self).await?);
        }
        Ok(guard)
    }

    pub fn chat(self: &Arc<Self>, input: ChatInput) -> DeltaStream {
        let this = self.clone();
        let (tx, rx) = mpsc::channel::<Result<String, ProviderError>>(64);
        let prompt = match input.system {
            Some(s) => format!("{s}\n\n{}", input.prompt),
            None => input.prompt,
        };
        let timeout = Duration::from_secs(self.cfg.timeout_secs.max(1));
        // Images become temp files the sidecar uploads; the paths are those inside its runtime.
        let mut launch = Launch::new(&self.cfg);
        let files: Result<Vec<String>, ProviderError> = input
            .images
            .iter()
            .map(|img| launch.temp_file(&img.format, &img.bytes))
            .collect();
        let temp = launch.temp_files();
        tokio::spawn(async move {
            let files = match files {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            let mut guard = match this.acquire().await {
                Ok(g) => g,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            let id = uuid::Uuid::new_v4().simple().to_string();
            let request = json!({ "op": "chat", "id": id, "prompt": prompt, "files": files, "timeout_ms": timeout.as_millis() as u64 });
            let outcome = run(guard.as_mut().expect("started"), request, &id, timeout, &tx).await;
            drop(temp);
            if let Err(e) = outcome {
                tracing::warn!("{e}; restarting sidecar on next request");
                *guard = None;
                let _ = tx.send(Err(e)).await;
            }
        });
        Box::pin(ReceiverStream::new(rx))
    }

    /// Opens a visible browser for the user to log in, then returns to headless.
    pub async fn login(self: &Arc<Self>) -> Result<(), ProviderError> {
        let mut guard = self.acquire().await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, mut rx) = mpsc::channel(8);
        let outcome = run(
            guard.as_mut().expect("started"),
            json!({ "op": "login", "id": id }),
            &id,
            Duration::from_secs(20 * 60),
            &tx,
        )
        .await;
        rx.close();
        if outcome.is_err() {
            *guard = None;
        }
        outcome
    }
}

/// Sends one request and forwards its deltas until `done`. An `Err` means the
/// sidecar is unusable and must be restarted; a provider-level failure
/// (`error` message) is delivered through `tx` and returns `Ok`.
async fn run(
    sc: &mut Sidecar,
    request: serde_json::Value,
    id: &str,
    timeout: Duration,
    tx: &mpsc::Sender<Result<String, ProviderError>>,
) -> Result<(), ProviderError> {
    sc.send(request).await?;
    let deadline = tokio::time::Instant::now() + timeout + Duration::from_secs(30);
    let mut client_gone = false;
    loop {
        let msg = tokio::time::timeout_at(deadline, sc.next())
            .await
            .map_err(|_| ProviderError("deepseek: sidecar stopped answering".into()))??;
        if msg.id != id {
            continue;
        }
        match msg.kind.as_str() {
            "delta" => {
                if !client_gone && tx.send(Ok(msg.text)).await.is_err() {
                    client_gone = true;
                    sc.send(json!({ "op": "cancel", "id": id })).await?;
                }
            }
            "done" => return Ok(()),
            "error" => {
                let _ = tx
                    .send(Err(ProviderError(format!("deepseek: {}", msg.message))))
                    .await;
                return Ok(());
            }
            _ => {}
        }
    }
}
