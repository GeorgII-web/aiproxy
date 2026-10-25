//! Child-process plumbing shared by the providers.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

use super::ProviderError;
use crate::config::{ProviderConfig, Runtime};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What to run, independent of where: natively or inside WSL.
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub runtime: Runtime,
    pub distro: String,
}

impl Launch {
    pub fn new(cfg: &ProviderConfig) -> Self {
        Self {
            program: cfg.command().to_string(),
            args: Vec::new(),
            cwd: (!cfg.workdir.is_empty()).then(|| cfg.workdir.clone()),
            env: Vec::new(),
            runtime: if cfg!(windows) { cfg.runtime } else { Runtime::Native },
            distro: cfg.wsl_distro.clone(),
        }
    }

    pub fn is_wsl(&self) -> bool {
        self.runtime == Runtime::Wsl
    }

    pub fn arg(&mut self, a: impl Into<String>) -> &mut Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(&mut self, items: I) -> &mut Self {
        self.args.extend(items.into_iter().map(Into::into));
        self
    }

    pub fn env(&mut self, k: &str, v: impl Into<String>) -> &mut Self {
        self.env.push((k.to_string(), v.into()));
        self
    }

    /// A path usable inside the runtime: Windows paths become `/mnt/<drive>/...` for WSL.
    pub fn path(&self, p: &Path) -> String {
        if self.is_wsl() {
            to_wsl_path(p)
        } else {
            p.to_string_lossy().into_owned()
        }
    }

    /// Builds the process. WSL: `wsl.exe [-d distro] [--cd dir] --exec [/usr/bin/env K=V..] program args..`,
    /// a bare program name resolved once through the user's login shell (PATH of `.profile`/`.bashrc`).
    pub async fn command(&self) -> Command {
        if !self.is_wsl() {
            let mut cmd = command(&self.program);
            cmd.args(&self.args).envs(self.env.iter().map(|(k, v)| (k, v)));
            if let Some(d) = &self.cwd {
                cmd.current_dir(d);
            }
            return cmd;
        }
        let program = if self.program.contains('/') {
            self.program.clone()
        } else {
            resolve_wsl(&self.distro, &self.program).await.unwrap_or_else(|| self.program.clone())
        };
        let mut cmd = command("wsl.exe");
        if !self.distro.is_empty() {
            cmd.args(["-d", &self.distro]);
        }
        if let Some(d) = &self.cwd {
            cmd.args(["--cd", d]);
        }
        cmd.arg("--exec");
        if !self.env.is_empty() {
            cmd.arg("/usr/bin/env");
            cmd.args(self.env.iter().map(|(k, v)| format!("{k}={v}")));
        }
        cmd.arg(program).args(&self.args);
        cmd
    }
}

/// `C:\a\b` → `/mnt/c/a/b`; Linux paths pass through.
pub fn to_wsl_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let rest = s[2..].trim_start_matches('/');
        return format!("/mnt/{}/{rest}", bytes[0].to_ascii_lowercase() as char);
    }
    s
}

/// `command -v <name>` inside WSL, cached per (distro, name). Uses an interactive login shell
/// because nvm and `~/.local/bin` are typically only on the PATH there.
async fn resolve_wsl(distro: &str, name: &str) -> Option<String> {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<String, String>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = format!("{distro}\0{name}");
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return Some(hit.clone());
    }
    let mut cmd = command("wsl.exe");
    if !distro.is_empty() {
        cmd.args(["-d", distro]);
    }
    cmd.args(["--exec", "/bin/bash", "-ilc", &format!("command -v {name}")]);
    let out = tokio::time::timeout(std::time::Duration::from_secs(20), cmd.output()).await.ok()?.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let found = text.lines().map(str::trim).rfind(|l| l.starts_with('/'))?.to_string();
    tracing::info!("wsl: {name} = {found}");
    cache.lock().unwrap().insert(key, found.clone());
    Some(found)
}

/// A command that is killed when dropped and opens no console window.
pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(resolve(program));
    cmd.kill_on_drop(true).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// On Windows a bare name like `claude` is looked up on PATH as `.exe`, `.cmd`
/// or `.bat` (npm installs `claude.cmd`, which CreateProcess would not find).
pub fn resolve(program: &str) -> PathBuf {
    let p = Path::new(program);
    if !cfg!(windows) || p.components().count() > 1 || p.extension().is_some() {
        return p.to_path_buf();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for ext in ["exe", "cmd", "bat"] {
                let candidate = dir.join(format!("{program}.{ext}"));
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    p.to_path_buf()
}

/// Kills the whole process tree on drop: the CLIs are launchers whose real
/// worker would otherwise keep running after a client disconnects.
pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        kill_tree(&mut self.0);
    }
}

pub fn kill_tree(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill").args(["-9", &format!("-{pid}")]).status();
    }
    let _ = child.start_kill();
}

pub struct Running {
    pub program: String,
    pub child: ChildGuard,
    pub lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
}

/// Spawns the command, optionally feeds `stdin`, and collects stderr in the background.
pub async fn spawn(mut cmd: Command, stdin: Option<String>) -> Result<Running, ProviderError> {
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .spawn()
        .map_err(|e| ProviderError(format!("cannot start {program}: {e}")))?;
    if let (Some(text), Some(mut input)) = (stdin, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = input.write_all(text.as_bytes()).await;
            let _ = input.shutdown().await;
        });
    }
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let buf = Arc::new(Mutex::new(String::new()));
    let sink = buf.clone();
    let tag = program.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "stderr", "{tag}: {line}");
            let mut b = sink.lock().unwrap();
            b.push_str(&line);
            b.push('\n');
            if b.len() > 8000 {
                let cut = (b.len() - 4000..b.len()).find(|&i| b.is_char_boundary(i)).unwrap_or(0);
                b.drain(..cut);
            }
        }
    });
    Ok(Running { program, child: ChildGuard(child), lines: BufReader::new(stdout).lines(), stderr: buf })
}

impl Running {
    /// Last 500 characters of stderr, for error messages.
    pub fn stderr_tail(&self) -> String {
        let s = self.stderr.lock().unwrap();
        let tail: Vec<char> = s.trim().chars().rev().take(500).collect();
        tail.into_iter().rev().collect()
    }
}
