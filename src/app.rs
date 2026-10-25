//! Shared state: settings, providers, the running HTTP server.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::autostart;
use crate::config::{Paths, Settings};
use crate::logging::Logging;
use crate::providers::Registry;
use crate::server;

pub struct AppState {
    pub paths: Paths,
    pub settings: RwLock<Settings>,
    pub registry: RwLock<Arc<Registry>>,
    pub log: Logging,
    server: Mutex<Option<ServerRun>>,
}

struct ServerRun {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl AppState {
    pub fn new(paths: Paths, settings: Settings, log: Logging) -> Arc<Self> {
        let registry = Arc::new(Registry::build(&settings, &paths));
        Arc::new(Self {
            paths,
            settings: RwLock::new(settings),
            registry: RwLock::new(registry),
            log,
            server: Mutex::new(None),
        })
    }

    pub fn base_url(&self) -> String {
        let s = self.settings.read().unwrap();
        format!("http://{}:{}", s.server.host, s.server.port)
    }

    /// Binds the configured address; stops a previous listener first.
    pub async fn start_server(self: &Arc<Self>) -> Result<SocketAddr> {
        let mut slot = self.server.lock().await;
        if let Some(run) = slot.take() {
            let _ = run.stop.send(());
            let _ = run.task.await;
        }
        let (host, port) = {
            let s = self.settings.read().unwrap();
            (s.server.host.clone(), s.server.port)
        };
        let listener = TcpListener::bind((host.as_str(), port))
            .await
            .with_context(|| format!("cannot listen on {host}:{port}"))?;
        let addr = listener.local_addr()?;
        let (stop, rx) = oneshot::channel();
        let router = server::router(self.clone());
        let task = tokio::spawn(async move {
            let serve = axum::serve(listener, router).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            if let Err(e) = serve.await {
                tracing::error!("server: {e}");
            }
        });
        tracing::info!("listening on http://{addr}/v1");
        *slot = Some(ServerRun { stop, task });
        Ok(addr)
    }

    /// Restart from inside a request: the old server must finish that request first.
    pub fn restart_server_later(self: &Arc<Self>) {
        let state = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Err(e) = state.start_server().await {
                tracing::error!("{e:#}");
            }
        });
    }

    /// Validates, saves and applies new settings. Returns whether the server is restarting.
    pub fn apply_settings(self: &Arc<Self>, new: Settings) -> Result<bool> {
        new.validate()?;
        new.save(&self.paths)?;
        let old = std::mem::replace(&mut *self.settings.write().unwrap(), new.clone());
        if old.logging.level != new.logging.level {
            self.log.set_level(&new.logging.level)?;
        }
        *self.registry.write().unwrap() = Arc::new(Registry::build(&new, &self.paths));
        if old.app.autostart != new.app.autostart {
            if let Err(e) = autostart::set(new.app.autostart) {
                tracing::warn!("{e:#}");
            }
        }
        let restart = old.server.host != new.server.host || old.server.port != new.server.port;
        if restart {
            self.restart_server_later();
        }
        tracing::info!(restart, "settings applied");
        Ok(restart)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_autostart(self: &Arc<Self>, enabled: bool) -> Result<()> {
        let mut s = self.settings.read().unwrap().clone();
        s.app.autostart = enabled;
        self.apply_settings(s).map(|_| ())
    }

    pub async fn shutdown(&self) {
        if let Some(run) = self.server.lock().await.take() {
            let _ = run.stop.send(());
            let _ = tokio::time::timeout(Duration::from_secs(2), run.task).await;
        }
        *self.registry.write().unwrap() = Arc::new(Registry::build(&Settings { providers: vec![], ..Default::default() }, &self.paths));
    }
}
