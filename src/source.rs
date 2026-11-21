//! Local journalctl follow and replay of the same newline-delimited JSON.

use crate::journal::{self, Frame, LineBuffer, MAX_LINE_BYTES};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{Mode, OFlags},
    io::Errno,
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    thread::{self, JoinHandle},
    time::Duration,
};

pub const DEFAULT_BACKLOG: u32 = 200;
const QUEUE_DEPTH: usize = 32;
const DIAGNOSTIC_LIMIT: usize = 2 * 1024;

#[derive(Clone, Debug)]
pub struct JournalFilters {
    pub backlog: u32,
    pub units: Vec<String>,
    pub user_units: Vec<String>,
    pub boot: Option<String>,
    pub priority: Option<String>,
}

impl Default for JournalFilters {
    fn default() -> Self {
        Self {
            backlog: DEFAULT_BACKLOG,
            units: Vec::new(),
            user_units: Vec::new(),
            boot: None,
            priority: None,
        }
    }
}

impl JournalFilters {
    pub fn args(&self) -> Vec<OsString> {
        let mut args = vec![
            "--follow".into(),
            "--output=json".into(),
            "--no-pager".into(),
            format!("--lines={}", self.backlog).into(),
        ];
        args.extend(
            self.units
                .iter()
                .map(|unit| format!("--unit={unit}").into()),
        );
        args.extend(
            self.user_units
                .iter()
                .map(|unit| format!("--user-unit={unit}").into()),
        );
        if let Some(boot) = &self.boot {
            args.push(format!("--boot={boot}").into());
        }
        if let Some(priority) = &self.priority {
            args.push(format!("--priority={priority}").into());
        }
        args
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceMessage {
    Frame(Frame),
    Diagnostic(String),
    Finished,
    Error(String),
}

/// A source owns its worker and any journalctl child process. Dropping it
/// cancels a blocked read or send and waits for the child to be reaped.
pub struct Source {
    receiver: Option<Receiver<SourceMessage>>,
    cancel: Option<UnixStream>,
    worker: Option<JoinHandle<()>>,
    child_id: Option<u32>,
}

impl Source {
    pub fn live(filters: &JournalFilters) -> io::Result<Self> {
        Self::live_with(OsStr::new("journalctl"), filters)
    }

    /// Select the journalctl executable for subprocess tests.
    pub fn live_with(program: &OsStr, filters: &JournalFilters) -> io::Result<Self> {
        let mut child = ChildGuard(
            Command::new(program)
                .args(filters.args())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let stdout = child.0.stdout.take().expect("piped child stdout");
        let stderr = child.0.stderr.take().expect("piped child stderr");
        Self::start(
            Stream::journal(File::from(OwnedFd::from(stdout))),
            Some(Stream::diagnostics(File::from(OwnedFd::from(stderr)))),
            Some(child),
        )
    }

    pub fn input(path: &Path) -> io::Result<Self> {
        let fd = if path == Path::new("-") {
            io::stdin().as_fd().try_clone_to_owned()?
        } else {
            rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )?
        };
        Self::start(Stream::journal(File::from(fd)), None, None)
    }

    fn start(
        input: Stream,
        diagnostics: Option<Stream>,
        mut child: Option<ChildGuard>,
    ) -> io::Result<Self> {
        let child_id = child.as_ref().map(|process| process.0.id());
        let (cancel, canceled) = UnixStream::pair()?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_DEPTH);
        let worker = thread::Builder::new()
            .name("jscope-intake".into())
            .spawn(move || {
                let result = pump(input, diagnostics, &canceled, &sender).and_then(|complete| {
                    if !complete {
                        return Ok(None);
                    }
                    match child.as_mut() {
                        None => Ok(Some(SourceMessage::Finished)),
                        Some(process) => {
                            Ok(wait_for_child(&mut process.0, &canceled)?.map(|status| {
                                if status.success() {
                                    SourceMessage::Finished
                                } else {
                                    SourceMessage::Error(format!("journalctl exited ({status})"))
                                }
                            }))
                        }
                    }
                });

                // Kill and reap on I/O failure as well as on cancellation.
                drop(child);

                let message = match result {
                    Ok(message) => message,
                    Err(error) => {
                        let origin = if child_id.is_some() {
                            "journalctl"
                        } else {
                            "input"
                        };
                        Some(SourceMessage::Error(format!(
                            "{origin}: {}",
                            journal::sanitize(&error.to_string())
                        )))
                    }
                };
                if let Some(message) = message {
                    let _ = sender.send(message);
                }
            })?;
        Ok(Self {
            receiver: Some(receiver),
            cancel: Some(cancel),
            worker: Some(worker),
            child_id,
        })
    }

    /// PID at spawn time, which may already have exited.
    pub fn child_id(&self) -> Option<u32> {
        self.child_id
    }

    pub fn try_recv(&self) -> Result<SourceMessage, TryRecvError> {
        self.receiver.as_ref().expect("active receiver").try_recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<SourceMessage, RecvTimeoutError> {
        self.receiver
            .as_ref()
            .expect("active receiver")
            .recv_timeout(timeout)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        // Disconnect the channel first so the worker can leave a blocked send,
        // then close the control socket to wake a blocked poll.
        drop(self.receiver.take());
        drop(self.cancel.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Stream {
    file: File,
    line: LineBuffer,
    decode: fn(Option<&[u8]>) -> SourceMessage,
}

enum Progress {
    More,
    Eof,
    Disconnected,
}

impl Stream {
    fn journal(file: File) -> Self {
        Self {
            file,
            line: LineBuffer::new(MAX_LINE_BYTES),
            decode: |line| SourceMessage::Frame(journal::record(line)),
        }
    }

    fn diagnostics(file: File) -> Self {
        Self {
            file,
            line: LineBuffer::new(DIAGNOSTIC_LIMIT),
            decode: |line| {
                let text = line.map_or_else(
                    || format!("journalctl stderr line exceeded {DIAGNOSTIC_LIMIT} bytes"),
                    |bytes| {
                        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
                        journal::sanitize(&String::from_utf8_lossy(bytes))
                    },
                );
                SourceMessage::Diagnostic(text)
            },
        }
    }

    fn read_ready(&mut self, sender: &SyncSender<SourceMessage>) -> io::Result<Progress> {
        let mut bytes = [0; 8192];
        let count = match self.file.read(&mut bytes) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(Progress::More);
            }
            Err(error) => return Err(error),
        };
        if count == 0 {
            if self.line.is_pending() && sender.send(self.line.take(self.decode)).is_err() {
                return Ok(Progress::Disconnected);
            }
            return Ok(Progress::Eof);
        }

        let mut chunk = &bytes[..count];
        while !chunk.is_empty() {
            let (consumed, complete) = self.line.push(chunk);
            chunk = &chunk[consumed..];
            if complete && sender.send(self.line.take(self.decode)).is_err() {
                return Ok(Progress::Disconnected);
            }
        }
        Ok(Progress::More)
    }
}

fn pump(
    input: Stream,
    mut diagnostics: Option<Stream>,
    canceled: &UnixStream,
    sender: &SyncSender<SourceMessage>,
) -> io::Result<bool> {
    let mut input = Some(input);
    while input.is_some() || diagnostics.is_some() {
        let mut fds = [
            PollFd::new(canceled, PollFlags::IN),
            poll_stream(input.as_ref(), canceled),
            poll_stream(diagnostics.as_ref(), canceled),
        ];
        poll_retry(&mut fds, None)?;
        if !fds[0].revents().is_empty() {
            return Ok(false);
        }
        let input_ready = !fds[1].revents().is_empty();
        let diagnostics_ready = !fds[2].revents().is_empty();

        for (ready, stream) in [
            (diagnostics_ready, &mut diagnostics),
            (input_ready, &mut input),
        ] {
            if ready && let Some(active) = stream {
                match active.read_ready(sender)? {
                    Progress::More => {}
                    Progress::Eof => *stream = None,
                    Progress::Disconnected => return Ok(false),
                }
            }
        }
    }
    Ok(true)
}

fn poll_stream<'a>(stream: Option<&'a Stream>, canceled: &'a UnixStream) -> PollFd<'a> {
    let fd = stream.map_or_else(|| canceled.as_fd(), |stream| stream.file.as_fd());
    PollFd::from_borrowed_fd(fd, PollFlags::IN)
}

fn poll_retry(fds: &mut [PollFd<'_>], timeout: Option<&Timespec>) -> io::Result<usize> {
    loop {
        match poll(fds, timeout) {
            Err(Errno::INTR) => continue,
            result => return result.map_err(Into::into),
        }
    }
}

fn wait_for_child(child: &mut Child, canceled: &UnixStream) -> io::Result<Option<ExitStatus>> {
    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 25_000_000,
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if poll_retry(&mut [PollFd::new(canceled, PollFlags::IN)], Some(&timeout))? > 0 {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_filters_use_single_arguments() {
        let options = JournalFilters {
            backlog: 0,
            units: vec!["alpha*.service".into(), "--priority=debug".into()],
            user_units: vec!["app@*.service".into()],
            boot: Some("-1".into()),
            priority: Some("err..warning".into()),
        };
        assert_eq!(
            options.args(),
            [
                "--follow",
                "--output=json",
                "--no-pager",
                "--lines=0",
                "--unit=alpha*.service",
                "--unit=--priority=debug",
                "--user-unit=app@*.service",
                "--boot=-1",
                "--priority=err..warning",
            ]
            .map(OsString::from)
        );
    }
}
