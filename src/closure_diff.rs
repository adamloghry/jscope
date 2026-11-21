//! One explicitly requested, locally available Nix closure comparison.
use jscope::journal::{Generation, is_store_path, sanitize};
use std::{
    io::{self, Read},
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 96 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct ClosureDiff {
    pub lines: Vec<String>,
    pub scroll: usize,
    job: Option<Job>,
}

struct Job {
    child: Child,
    reader: UnixStream,
    output: Vec<u8>,
    started: Instant,
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ClosureDiff {
    pub fn start(marker: &Generation) -> io::Result<Self> {
        let old = marker.previous.as_deref().ok_or_else(|| {
            io::Error::other("First activation has no previous system to compare")
        })?;
        let new = marker.current.as_str();
        for path in [old, new] {
            if !is_store_path(path) {
                return Err(io::Error::other("Marker has an invalid Nix store path"));
            }
            if !Path::new(path).is_dir() {
                return Err(io::Error::other(format!(
                    "Closure unavailable in this machine’s store: {path}"
                )));
            }
        }
        let mut cmd = Command::new("nix");
        cmd.args([
            "--offline",
            "--store",
            "auto",
            "--extra-experimental-features",
            "nix-command",
            "store",
            "diff-closures",
            old,
            new,
        ])
        .env("NO_COLOR", "1");
        Self::spawn(cmd, old, new)
    }

    fn spawn(mut cmd: Command, old: &str, new: &str) -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        let stderr = OwnedFd::from(writer.try_clone()?);
        let child = cmd
            .stdin(Stdio::null())
            .stdout(OwnedFd::from(writer))
            .stderr(stderr)
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("Starting closure comparison: {e}")))?;
        Ok(Self {
            lines: vec![
                format!("From {old}"),
                format!("To   {new}"),
                "Comparing local closures…".into(),
            ],
            scroll: 0,
            job: Some(Job {
                child,
                reader,
                output: Vec::new(),
                started: Instant::now(),
            }),
        })
    }

    pub fn poll(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else {
            return false;
        };
        let result = job.read_result();
        let Some(result) = result else { return false };
        self.lines.truncate(2);
        match result {
            Ok(()) if job.output.is_empty() => {
                self.lines.push("No package changes reported".into())
            }
            Ok(()) => (),
            Err(reason) => self.lines.push(reason),
        }
        self.lines
            .extend(String::from_utf8_lossy(&job.output).lines().map(sanitize));
        self.job = None;
        true
    }
}

impl Job {
    fn read_result(&mut self) -> Option<Result<(), String>> {
        if self.started.elapsed() >= TIMEOUT {
            return Some(Err("Comparison timed out after 30 seconds".into()));
        }
        let mut chunk = [0u8; 8192];
        loop {
            match self.reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let remaining = OUTPUT_LIMIT - self.output.len();
                    self.output.extend_from_slice(&chunk[..n.min(remaining)]);
                    if n > remaining {
                        return Some(Err("Comparison output truncated at 96 KiB".into()));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Some(Err(e.to_string())),
            }
        }
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) if status.success() => Some(Ok(())),
            Ok(Some(status)) => Some(Err(format!("Closure comparison exited with {status}"))),
            Err(e) => Some(Err(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsStr, thread};

    fn command(program: &str, args: &[&str]) -> ClosureDiff {
        let mut command = Command::new(OsStr::new(program));
        command.args(args);
        ClosureDiff::spawn(command, "old", "new").unwrap()
    }

    fn finish(diff: &mut ClosureDiff) {
        let started = Instant::now();
        while !diff.poll() {
            assert!(started.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn output_is_bounded_and_escaped_and_errors_are_visible() {
        let mut diff = command("printf", &["bad\\033[2J\\n"]);
        finish(&mut diff);
        assert!(diff.lines.iter().any(|s| s.contains(r"\x1b[2J")));
        let mut diff = command("yes", &["package"]);
        finish(&mut diff);
        assert!(diff.lines[2].contains("truncated"));
        let mut diff = command("false", &[]);
        finish(&mut diff);
        assert!(diff.lines[2].contains("exited"));
    }

    #[test]
    fn closing_reaps_a_comparison_in_progress() {
        let diff = command("sleep", &["60"]);
        let pid = diff.job.as_ref().unwrap().child.id();
        drop(diff);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
}
