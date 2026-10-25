//! AIProxy: a tray app that serves an OpenAI-compatible API on top of local
//! AI agents. See README.md.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autostart;
mod config;
mod logging;
mod openai;
mod providers;
mod server;
#[cfg(windows)]
mod tray;

use anyhow::Result;

fn main() -> Result<()> {
    let paths = config::Paths::new()?;
    let settings = config::Settings::load_or_create(&paths)?;
    let log = logging::init(&paths, &settings.logging)?;
    tracing::info!(config = %paths.config_file.display(), "aiproxy {}", env!("CARGO_PKG_VERSION"));
    autostart::sync(settings.app.autostart);

    let rt = tokio::runtime::Runtime::new()?;
    let state = app::AppState::new(paths, settings, log, rt.handle().clone());
    if let Err(e) = rt.block_on(state.start_server()) {
        tracing::error!("{e:#}");
    }

    #[cfg(windows)]
    {
        tray::run(state, rt)
    }
    #[cfg(not(windows))]
    {
        rt.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
            state.shutdown().await;
        });
        Ok(())
    }
}
