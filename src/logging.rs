//! Daily log files in `<home>/logs`, level switchable at runtime.

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, Registry};

use crate::config::{LoggingSettings, Paths};

pub struct Logging {
    reload: reload::Handle<EnvFilter, Registry>,
    _guard: tracing_appender::non_blocking::WorkerGuard,
    pub dir: PathBuf,
}

fn filter(level: &str) -> Result<EnvFilter> {
    let level = if level.trim().is_empty() {
        "info"
    } else {
        level.trim()
    };
    EnvFilter::try_new(format!("{level},hyper_util=warn,h2=warn,tower_http=warn"))
        .with_context(|| format!("invalid log level {level:?}"))
}

pub fn init(paths: &Paths, cfg: &LoggingSettings) -> Result<Logging> {
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("aiproxy")
        .filename_suffix("log")
        .max_log_files(cfg.keep_files.max(1))
        .build(&paths.logs_dir)?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let (layer, reload) = reload::Layer::new(filter(&cfg.level)?);
    tracing_subscriber::registry()
        .with(layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .init();
    Ok(Logging {
        reload,
        _guard: guard,
        dir: paths.logs_dir.clone(),
    })
}

impl Logging {
    pub fn set_level(&self, level: &str) -> Result<()> {
        let f = filter(level)?;
        self.reload.reload(f).context("cannot switch log level")
    }

    /// Last `lines` lines of the newest log file.
    pub fn tail(&self, lines: usize) -> Result<String> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("aiproxy.") && n.ends_with(".log"))
            })
            .collect();
        files.sort();
        let Some(newest) = files.pop() else {
            return Ok(String::new());
        };
        let text = std::fs::read_to_string(&newest)?;
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        Ok(all[start..].join("\n"))
    }
}
