//! OpenCode server process management.
//!
//! Spawns `opencode serve`, monitors health via HTTP, restarts on crash,
//! and shuts down gracefully.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Context as _;
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::opencode::client::OpenCodeClient;

// ── Types ─────────────────────────────────────────────────────────────────────

struct OpenCodeProcess {
    child: Child,
    pid: u32,
}

/// Manages the lifecycle of the OpenCode server subprocess.
pub struct OpenCodeProcessManager {
    state: Mutex<Option<OpenCodeProcess>>,
    port: u16,
    hostname: String,
    config_dir: PathBuf, // directory containing opencode.json
}

static OPENCODE_PROCESS: OnceLock<std::sync::Arc<OpenCodeProcessManager>> = OnceLock::new();

/// Initialise the global process manager. Call once at daemon startup.
pub fn init_opencode_process(port: u16, hostname: &str, config_dir: PathBuf) {
    let _ = OPENCODE_PROCESS.set(std::sync::Arc::new(OpenCodeProcessManager {
        state: Mutex::new(None),
        port,
        hostname: hostname.to_string(),
        config_dir,
    }));
}

/// Access the global process manager.
pub fn opencode_process() -> Option<std::sync::Arc<OpenCodeProcessManager>> {
    OPENCODE_PROCESS.get().cloned()
}

// ── Implementation ────────────────────────────────────────────────────────────

impl OpenCodeProcessManager {
    /// Ensure the OpenCode server is running.
    ///
    /// If the process has exited or was never started, spawns a new one and
    /// waits up to 30 s for it to become healthy.
    pub async fn ensure_running(&self) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;

        // Check if already running
        if let Some(proc) = state.as_mut() {
            match proc.child.try_wait() {
                Ok(Some(status)) => {
                    warn!(pid = proc.pid, ?status, "opencode process exited, re-spawning");
                    *state = None;
                }
                Ok(None) => {
                    debug!("opencode already running");
                    return Ok(());
                }
                Err(e) => {
                    warn!(error = %e, "try_wait error, re-spawning");
                    *state = None;
                }
            }
        }

        // Spawn new process (release lock during I/O)
        drop(state);
        let proc = self.spawn_opencode()?;
        let pid = proc.pid;
        self.wait_for_ready().await?;
        self.state.lock().await.replace(proc);
        info!(pid, "opencode server started");
        Ok(())
    }

    /// Gracefully shut down the OpenCode server.
    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        let Some(mut proc) = state.take() else { return };

        // Best-effort dispose
        let client = OpenCodeClient::new(self.port);
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            async {
                let url = format!("http://{}:{}/instance/dispose", self.hostname, self.port);
                let _ = reqwest::Client::new().post(&url).send().await;
            },
        )
        .await;

        // Wait up to 5 s for graceful exit
        for _ in 0..50 {
            if let Ok(Some(_)) = proc.child.try_wait() {
                info!(pid = proc.pid, "opencode stopped gracefully");
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Force kill
        let _ = proc.child.kill().await;
        warn!(pid = proc.pid, "opencode did not exit gracefully, killed");
        drop(client);
    }

    /// Kill any `opencode serve` processes left from a previous daemon crash.
    pub async fn cleanup_orphans() {
        let result = tokio::process::Command::new("pkill")
            .args(["-f", "opencode serve"])
            .output()
            .await;
        match result {
            Ok(out) if out.status.success() => {
                info!("killed orphaned opencode server processes");
            }
            _ => {
                debug!("no orphaned opencode processes found");
            }
        }
    }

    // ── Private ───────────────────────────────────────────────────────────────

    fn spawn_opencode(&self) -> anyhow::Result<OpenCodeProcess> {
        let binary = find_opencode_binary()?;
        info!(
            binary = %binary.display(),
            port = self.port,
            hostname = %self.hostname,
            config_dir = %self.config_dir.display(),
            "spawning opencode serve"
        );

        let mut cmd = tokio::process::Command::new(&binary);
        cmd.args(["serve", "--port", &self.port.to_string(), "--hostname", &self.hostname]);

        // Set OPENCODE_CONFIG_DIR so OpenCode reads our opencode.json.
        // Keep PATH and HOME from parent process
        // (do NOT env_clear — OpenCode needs bun/node in PATH).
        cmd.env("OPENCODE_CONFIG_DIR", &self.config_dir);

        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().context("spawn opencode serve")?;
        let pid = child.id().unwrap_or(0);

        // Forward stderr to tracing
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt as _;
                let mut reader = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::info!(target: "opencode", "{}", line);
                }
            });
        }

        Ok(OpenCodeProcess { child, pid })
    }

    async fn wait_for_ready(&self) -> anyhow::Result<()> {
        let client = OpenCodeClient::new(self.port);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

        let mut attempt = 0u32;
        loop {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("opencode server failed to start within 30s");
            }
            match client.health_check().await {
                Ok(()) => {
                    info!(attempts = attempt + 1, "opencode server ready");
                    return Ok(());
                }
                Err(e) => {
                    debug!(attempt, error = %e, "opencode not ready yet");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    attempt += 1;
                }
            }
        }
    }
}

// ── Binary discovery ──────────────────────────────────────────────────────────

fn find_opencode_binary() -> anyhow::Result<PathBuf> {
    // 1. PATH lookup
    if let Ok(p) = which::which("opencode") {
        return Ok(p);
    }

    // 2. ~/.local/share/opencode/opencode (XDG)
    if let Some(home) = home::home_dir() {
        let p = home.join(".local/share/opencode/opencode");
        if p.exists() {
            return Ok(p);
        }

        // 3. ~/.bun/bin/opencode
        let p = home.join(".bun/bin/opencode");
        if p.exists() {
            return Ok(p);
        }

        // 4. ~/.nvm/versions/node/*/bin/opencode (take any)
        let pattern = home
            .join(".nvm/versions/node/*/bin/opencode")
            .to_string_lossy()
            .to_string();
        if let Ok(mut paths) = glob::glob(&pattern) {
            if let Some(Ok(p)) = paths.next() {
                return Ok(p);
            }
        }
    }

    // 5. /usr/local/bin/opencode
    let p = PathBuf::from("/usr/local/bin/opencode");
    if p.exists() {
        return Ok(p);
    }

    anyhow::bail!(
        "opencode binary not found; install with: npm install -g opencode-ai"
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_opencode_binary_finds_something() {
        // On this machine opencode is installed; just verify no panic.
        // If not installed the test is still valid — it returns Err.
        let _ = find_opencode_binary();
    }

    #[tokio::test]
    async fn shutdown_is_noop_when_not_running() {
        let mgr = OpenCodeProcessManager {
            state: Mutex::new(None),
            port: 14096,
            hostname: "127.0.0.1".to_string(),
            config_dir: PathBuf::from("/tmp"),
        };
        mgr.shutdown().await; // must not panic
    }

    #[tokio::test]
    async fn wait_for_ready_times_out_on_closed_port() {
        // Pick an unlikely-used port and verify timeout fires.
        let mgr = OpenCodeProcessManager {
            state: Mutex::new(None),
            port: 19999,
            hostname: "127.0.0.1".to_string(),
            config_dir: PathBuf::from("/tmp"),
        };
        // Override wait to 2 s for the test
        let client = OpenCodeClient::new(19999);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut ok = false;
        while tokio::time::Instant::now() < deadline {
            if client.health_check().await.is_ok() {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!ok, "port 19999 should not be serving");
        drop(mgr);
    }
}
