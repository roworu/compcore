//! runs a program's help, with a deadline and a cap on what it keeps, for both the spec
//! generator and the runtime prober. only help is ever asked for, never the program's work

use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// how long a help run may take and how much of its output is kept
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub timeout: Duration,
    pub max_bytes: usize,
}

/// stdout and stderr of `program args`, in a plain locale and without a pager. empty when
/// the program can't be started, prints nothing or runs past the deadline
pub fn run_help(program: &Path, args: &[String], cwd: Option<&Path>, limits: Limits) -> String {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("COLUMNS", "120")
        .env("LINES", "1000")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("CLICOLOR", "0")
        .env("PAGER", "cat")
        .env("MANPAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("SYSTEMD_PAGER", "cat");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let Ok(mut child) = command.spawn() else {
        return String::new();
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        child.kill().ok();
        return String::new();
    };
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    read_capped(stdout, limits.max_bytes, out_tx);
    read_capped(stderr, limits.max_bytes, err_tx);
    let deadline = Instant::now() + limits.timeout;
    while let Ok(None) = child.try_wait() {
        if Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // a program left behind by the one asked may hold the pipes; completion must not wait
    let wait = deadline.saturating_duration_since(Instant::now());
    let stdout = out_rx.recv_timeout(wait).unwrap_or_default();
    let stderr = err_rx.recv_timeout(wait).unwrap_or_default();
    if stdout.is_empty() && stderr.is_empty() {
        return String::new();
    }
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    )
}

/// read a pipe on its own thread, keeping at most `max_bytes` so a chatty program can't fill
/// memory. reading goes on after the cap so the program never blocks on a full pipe
fn read_capped(mut pipe: impl Read + Send + 'static, max_bytes: usize, tx: mpsc::Sender<Vec<u8>>) {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let room = max_bytes.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..read.min(room)]);
                }
            }
        }
        tx.send(kept).ok();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, limits: Limits) -> String {
        run_help(Path::new("sh"), &["-c".into(), script.into()], None, limits)
    }

    #[test]
    fn both_streams_are_kept() {
        let limits = Limits {
            timeout: Duration::from_secs(2),
            max_bytes: 1000,
        };
        let out = sh("echo out; echo err >&2", limits);
        assert!(out.contains("out"), "{out}");
        assert!(out.contains("err"), "{out}");
    }

    #[test]
    fn output_is_capped_and_runs_end_at_the_deadline() {
        let out = sh(
            "yes x | head -c 100000",
            Limits {
                timeout: Duration::from_secs(2),
                max_bytes: 1000,
            },
        );
        assert!(out.len() <= 2001, "{}", out.len());
        let started = Instant::now();
        let out = sh(
            "sleep 10",
            Limits {
                timeout: Duration::from_millis(100),
                max_bytes: 1000,
            },
        );
        assert!(out.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
