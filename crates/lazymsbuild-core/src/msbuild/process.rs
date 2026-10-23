//! Running MSBuild (and the design-time extractor) as a child process.
//!
//! - The process starts suspended, goes into a Job Object that kills everything in it when it is
//!   closed, then resumes, so no worker node, compiler or `mspdbsrv.exe` it starts can escape.
//!   Cancelling terminates the job: nothing is left running.
//! - It gets its own hidden console (`CREATE_NO_WINDOW`): Ctrl+C in lazymsbuild's terminal does not
//!   reach it, and its output code page cannot disturb the terminal.
//! - Output is read line by line and decoded as UTF-8, or in `code_page` when not valid UTF-8.
//! - The run ends when the process exits, not when its output ends: helpers such as `vctip.exe`
//!   can keep inherited pipe handles open for minutes. Leftovers are ended with the job.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::text::{self, CodePage};

/// What to run.
#[derive(Clone, Debug)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub code_page: CodePage,
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// The exit code; `None` when the process was killed.
    pub code: Option<i32>,
    pub cancelled: bool,
}

impl Exit {
    pub fn success(&self) -> bool {
        !self.cancelled && self.code == Some(0)
    }
}

/// Runs a process to the end, sending each output line (stdout and stderr) to `lines`.
pub async fn run(spec: Spec, lines: mpsc::UnboundedSender<String>, cancel: CancellationToken) -> Result<Exit> {
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().with_context(|| format!("cannot start {}", spec.program.display()))?;

    #[cfg(windows)]
    let job = {
        let job = crate::win::Job::new().ok();
        if let (Some(job), Some(handle)) = (&job, child.raw_handle())
            && let Err(err) = job.assign(handle as _)
        {
            let _ = lines.send(format!(
                "lazymsbuild: could not contain the process tree ({err}); a cancel may leave processes behind"
            ));
        }
        if let Some(pid) = child.id()
            && let Err(err) = crate::win::resume_process(pid)
        {
            let _ = child.start_kill();
            return Err(err).context("cannot resume the started process");
        }
        job
    };

    let stdout = child.stdout.take().map(|s| tokio::spawn(forward(s, lines.clone(), spec.code_page)));
    let stderr = child.stderr.take().map(|s| tokio::spawn(forward(s, lines.clone(), spec.code_page)));

    let (status, cancelled) = tokio::select! {
        status = child.wait() => (Some(status.context("waiting for the process failed")?), false),
        _ = cancel.cancelled() => {
            #[cfg(windows)]
            if let Some(job) = &job {
                let _ = job.terminate();
            }
            let _ = child.start_kill();
            (child.wait().await.ok(), true)
        }
    };

    // The process is gone; end whatever it left (vctip.exe, mspdbsrv.exe) so the pipes close.
    #[cfg(windows)]
    drop(job);

    for reader in [stdout, stderr].into_iter().flatten() {
        let handle = reader.abort_handle();
        if tokio::time::timeout(Duration::from_secs(2), reader).await.is_err() {
            handle.abort();
        }
    }

    let code = if cancelled { None } else { status.and_then(|s| s.code()) };
    Ok(Exit { code, cancelled })
}

async fn forward<R: AsyncRead + Unpin>(stream: R, lines: mpsc::UnboundedSender<String>, code_page: CodePage) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::with_capacity(256);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                if lines.send(text::decode_line(&buf, code_page)).is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn cmd(script: &str) -> Spec {
        Spec {
            program: PathBuf::from(std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into())),
            args: vec!["/d".into(), "/c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env: vec![("LAZYMSBUILD_TEST".into(), "x".into())],
            code_page: CodePage::Oem,
        }
    }

    #[tokio::test]
    async fn streams_lines_and_reports_the_exit_code() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let exit =
            run(cmd("echo one& echo %LAZYMSBUILD_TEST%& exit /b 3"), tx, CancellationToken::new()).await.unwrap();
        assert_eq!(exit, Exit { code: Some(3), cancelled: false });
        let mut got = Vec::new();
        while let Ok(line) = rx.try_recv() {
            got.push(line);
        }
        assert_eq!(got, ["one", "x"]);
    }

    #[tokio::test]
    async fn cancel_ends_the_whole_tree() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            trigger.cancel();
        });
        let started = std::time::Instant::now();
        // A child that starts a grandchild, both waiting for a minute.
        let exit =
            run(cmd("start /b ping -n 60 127.0.0.1 >nul & ping -n 60 127.0.0.1 >nul"), tx, cancel).await.unwrap();
        assert!(exit.cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
