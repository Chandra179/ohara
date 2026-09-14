//! Child-process lifecycle and Linux resource measurement.

use crate::{Error, Result};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A real Ohara process started by a verification harness.
pub(crate) struct ManagedProcess {
    /// Human-readable process name used in logs and errors.
    pub(crate) name: String,
    /// Captured combined stdout and stderr log path.
    pub(crate) log_path: PathBuf,
    child: Child,
    log_file: Option<File>,
}

impl ManagedProcess {
    /// Start a debug binary from the workspace target directory.
    pub(crate) fn start(
        root: &Path,
        name: impl Into<String>,
        binary: &str,
        environment: &HashMap<String, String>,
        log_dir: &Path,
    ) -> Result<Self> {
        let name = name.into();
        std::fs::create_dir_all(log_dir).map_err(|source| {
            Error::io(
                format!("create log directory {}", log_dir.display()),
                source,
            )
        })?;
        let log_path = log_dir.join(format!("{name}.log"));
        let log_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&log_path)
            .map_err(|source| {
                Error::io(format!("open process log {}", log_path.display()), source)
            })?;
        let stdout = log_file
            .try_clone()
            .map_err(|source| Error::io("clone process log", source))?;
        let child = Command::new(root.join("target").join("debug").join(binary))
            .current_dir(root)
            .envs(environment)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log_file.try_clone().map_err(|source| {
                Error::io("clone process stderr log", source)
            })?))
            .spawn()
            .map_err(|source| Error::io(format!("start {name} ({binary})"), source))?;
        Ok(Self {
            name,
            log_path,
            child,
            log_file: Some(log_file),
        })
    }

    /// Return the operating-system process id.
    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Return whether the child has not exited yet.
    pub(crate) fn is_running(&mut self) -> Result<bool> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|source| Error::io(format!("poll {}", self.name), source))
    }

    /// Return the child exit code when it has exited.
    pub(crate) fn return_code(&mut self) -> Result<Option<i32>> {
        self.child
            .try_wait()
            .map(|status| status.and_then(|status| status.code()))
            .map_err(|source| Error::io(format!("poll {}", self.name), source))
    }

    /// Send SIGTERM, wait briefly, and force-kill a process that does not drain.
    pub(crate) async fn stop(&mut self) -> Result<()> {
        if self.is_running()? {
            terminate_process(self.pid())?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if self.return_code()?.is_some() {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    self.child
                        .kill()
                        .map_err(|source| Error::io(format!("kill {}", self.name), source))?;
                    self.child
                        .wait()
                        .map_err(|source| Error::io(format!("wait for {}", self.name), source))?;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        self.close_log();
        Ok(())
    }

    /// Read the captured combined output.
    pub(crate) fn log(&self) -> Result<String> {
        std::fs::read_to_string(&self.log_path).map_err(|source| {
            Error::io(
                format!("read process log {}", self.log_path.display()),
                source,
            )
        })
    }

    /// Close the log handle after the child has been stopped.
    pub(crate) fn close_log(&mut self) {
        self.log_file.take();
    }
}

fn terminate_process(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .map_err(|source| Error::io("send SIGTERM", source))?;
        if !status.success() {
            return Err(Error::Message(format!("kill -TERM {pid} failed")));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(Error::Message(
            "graceful process termination requires a Unix host".to_owned(),
        ))
    }
}

/// Read Linux's high-water resident set size in KiB.
pub(crate) fn peak_rss_kib(pid: u32) -> Result<Option<u64>> {
    let path = format!("/proc/{pid}/status");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::io(format!("read {path}"), source)),
    };
    Ok(contents.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some("VmHWM:")).then(|| fields.next()?.parse::<u64>().ok())?
    }))
}

/// Return inherited environment variables with supplied overrides.
pub(crate) fn environment(
    overrides: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, String> {
    let mut values: HashMap<String, String> = std::env::vars().collect();
    values.extend(overrides);
    values
}

/// Return an inherited environment with per-process overrides applied.
pub(crate) fn with_values(
    base: &HashMap<String, String>,
    overrides: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, String> {
    let mut values = base.clone();
    values.extend(overrides);
    values
}

#[cfg(all(test, unix))]
mod tests {
    use super::ManagedProcess;
    use std::path::PathBuf;
    use std::process::Command;

    #[tokio::test]
    async fn stop_sends_sigterm_and_reaps_child() -> Result<(), Box<dyn std::error::Error>> {
        let child = Command::new("sh")
            .args(["-c", "trap 'exit 0' TERM; sleep 30"])
            .spawn()?;
        let mut process = ManagedProcess {
            name: "shutdown-test".to_owned(),
            log_path: PathBuf::from("/tmp/ohara-tools-shutdown-test.log"),
            child,
            log_file: None,
        };

        process.stop().await?;

        assert!(!process.is_running()?);
        Ok(())
    }
}
