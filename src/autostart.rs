//! "Start with Windows" via the registry Run key (login item elsewhere).

use anyhow::{anyhow, Result};
use auto_launch::{AutoLaunch, AutoLaunchBuilder};

use crate::config::APP_NAME;

fn launcher() -> Result<AutoLaunch> {
    let exe = std::env::current_exe()?;
    AutoLaunchBuilder::new()
        .set_app_name(APP_NAME)
        .set_app_path(&exe.to_string_lossy())
        .build()
        .map_err(|e| anyhow!("autostart: {e}"))
}

pub fn is_enabled() -> bool {
    launcher()
        .and_then(|l| l.is_enabled().map_err(|e| anyhow!("{e}")))
        .unwrap_or(false)
}

pub fn set(enabled: bool) -> Result<()> {
    let l = launcher()?;
    let r = if enabled { l.enable() } else { l.disable() };
    r.map_err(|e| anyhow!("autostart: {e}"))
}

/// Make the OS entry match the setting (the exe may have moved since it was written).
pub fn sync(wanted: bool) {
    if wanted != is_enabled() {
        if let Err(e) = set(wanted) {
            tracing::warn!("{e:#}");
        }
    }
}
