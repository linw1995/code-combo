use std::{
    io,
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
    time::Duration,
};

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const HARD_MAX_BUFFERED_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputChunk {
    pub timestamp: i64,
    pub stream: StreamKind,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent {
    Started {
        timestamp: i64,
        argv: Vec<String>,
    },
    Chunk(OutputChunk),
    Exited {
        timestamp: i64,
        exit_code: Option<i32>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ChunkConfig {
    pub interval: Duration,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(500),
        }
    }
}

#[derive(Debug)]
pub struct ProcessEventStream {
    rx: mpsc::Receiver<ProcessEvent>,
}

impl Stream for ProcessEventStream {
    type Item = ProcessEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.rx.poll_recv(cx)
    }
}

#[derive(Debug)]
pub struct ProcessKiller {
    kill_tx: Option<oneshot::Sender<()>>,
}

impl ProcessKiller {
    pub fn kill(&mut self) {
        if let Some(tx) = self.kill_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Debug)]
pub struct RunningProcess {
    pub events: ProcessEventStream,
    pub killer: ProcessKiller,
    handle: JoinHandle<io::Result<std::process::ExitStatus>>,
}

impl RunningProcess {
    pub async fn wait(self) -> Result<std::process::ExitStatus, io::Error> {
        match self.handle.await {
            Ok(result) => result,
            Err(err) => Err(io::Error::other(err.to_string())),
        }
    }

    pub fn abort(&self) {
        self.handle.abort();
    }
}

#[derive(Debug)]
pub struct ExecCommand {
    argv: Vec<String>,
    envs: Vec<(String, String)>,
    stdin: Stdio,
}

impl ExecCommand {
    pub fn from_argv(argv: Vec<String>) -> Self {
        Self {
            argv,
            envs: Vec::new(),
            stdin: Stdio::null(),
        }
    }

    pub fn envs<I, K, V>(mut self, envs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.envs = envs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self
    }

    pub fn stdin(mut self, stdin: Stdio) -> Self {
        self.stdin = stdin;
        self
    }

    pub fn spawn_chunked(self, config: ChunkConfig) -> io::Result<RunningProcess> {
        if self.argv.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "argv must not be empty",
            ));
        }

        let mut cmd = Command::new(&self.argv[0]);
        if self.argv.len() > 1 {
            cmd.args(&self.argv[1..]);
        }
        cmd.stdin(self.stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .envs(self.envs.iter().map(|(k, v)| (k, v)));

        let mut child = cmd.spawn()?;
        let _ = child.stdin.take();

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("stdout was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("stderr was not captured"))?;

        let (event_tx, event_rx) = mpsc::channel(64);
        let (kill_tx, mut kill_rx) = oneshot::channel::<()>();

        let argv = self.argv.clone();
        let handle = tokio::spawn(async move {
            let started_ts = OffsetDateTime::now_utc().unix_timestamp();
            let _ = event_tx
                .send(ProcessEvent::Started {
                    timestamp: started_ts,
                    argv,
                })
                .await;

            let mut stdout_reader = BufReader::new(stdout).lines();
            let mut stderr_reader = BufReader::new(stderr).lines();

            let mut stdout_closed = false;
            let mut stderr_closed = false;
            let mut stdout_lines: Vec<String> = Vec::new();
            let mut stderr_lines: Vec<String> = Vec::new();
            let mut stdout_bytes: usize = 0;
            let mut stderr_bytes: usize = 0;
            let mut kill_seen = false;

            let mut ticker = if config.interval.is_zero() {
                None
            } else {
                Some(tokio::time::interval(config.interval))
            };

            async fn flush(
                tx: &mpsc::Sender<ProcessEvent>,
                stream: StreamKind,
                lines: &mut Vec<String>,
                bytes: &mut usize,
            ) {
                if lines.is_empty() {
                    return;
                }
                let timestamp = OffsetDateTime::now_utc().unix_timestamp();
                let payload = std::mem::take(lines);
                *bytes = 0;
                let _ = tx
                    .send(ProcessEvent::Chunk(OutputChunk {
                        timestamp,
                        stream,
                        lines: payload,
                    }))
                    .await;
            }

            loop {
                tokio::select! {
                    _ = &mut kill_rx, if !kill_seen => {
                        kill_seen = true;
                        let _ = child.start_kill();
                    }
                    line = stdout_reader.next_line(), if !stdout_closed => {
                        match line {
                            Ok(Some(line)) => {
                                stdout_bytes = stdout_bytes.saturating_add(line.len() + 1);
                                stdout_lines.push(line);
                                if config.interval.is_zero() || stdout_bytes >= HARD_MAX_BUFFERED_BYTES {
                                    flush(&event_tx, StreamKind::Stdout, &mut stdout_lines, &mut stdout_bytes).await;
                                }
                            }
                            Ok(None) => {
                                stdout_closed = true;
                            }
                            Err(_) => {
                                stdout_closed = true;
                            }
                        }
                    }
                    line = stderr_reader.next_line(), if !stderr_closed => {
                        match line {
                            Ok(Some(line)) => {
                                stderr_bytes = stderr_bytes.saturating_add(line.len() + 1);
                                stderr_lines.push(line);
                                if config.interval.is_zero() || stderr_bytes >= HARD_MAX_BUFFERED_BYTES {
                                    flush(&event_tx, StreamKind::Stderr, &mut stderr_lines, &mut stderr_bytes).await;
                                }
                            }
                            Ok(None) => {
                                stderr_closed = true;
                            }
                            Err(_) => {
                                stderr_closed = true;
                            }
                        }
                    }
                    _ = async {
                        match ticker.as_mut() {
                            Some(ticker) => ticker.tick().await,
                            None => std::future::pending().await,
                        }
                    }, if ticker.is_some() && (!stdout_closed || !stderr_closed) => {
                        flush(&event_tx, StreamKind::Stdout, &mut stdout_lines, &mut stdout_bytes).await;
                        flush(&event_tx, StreamKind::Stderr, &mut stderr_lines, &mut stderr_bytes).await;
                    }
                    else => break,
                }

                if stdout_closed && stderr_closed {
                    break;
                }
            }

            flush(
                &event_tx,
                StreamKind::Stdout,
                &mut stdout_lines,
                &mut stdout_bytes,
            )
            .await;
            flush(
                &event_tx,
                StreamKind::Stderr,
                &mut stderr_lines,
                &mut stderr_bytes,
            )
            .await;

            let status = child.wait().await?;
            let exited_ts = OffsetDateTime::now_utc().unix_timestamp();
            let _ = event_tx
                .send(ProcessEvent::Exited {
                    timestamp: exited_ts,
                    exit_code: status.code(),
                })
                .await;
            Ok(status)
        });

        Ok(RunningProcess {
            events: ProcessEventStream { rx: event_rx },
            killer: ProcessKiller {
                kill_tx: Some(kill_tx),
            },
            handle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn chunked_runner_emits_started_and_exited() -> Result<(), Box<dyn std::error::Error>> {
        let argv = vec![
            "bash".to_string(),
            "-c".to_string(),
            "echo out; echo err 1>&2".to_string(),
        ];
        let mut proc = ExecCommand::from_argv(argv).spawn_chunked(ChunkConfig {
            interval: Duration::from_millis(10),
        })?;

        let mut saw_started = false;
        let mut saw_out = false;
        let mut saw_err = false;
        let mut saw_exited = false;

        while let Some(ev) = proc.events.next().await {
            match ev {
                ProcessEvent::Started { timestamp, argv } => {
                    assert!(timestamp > 0);
                    assert!(!argv.is_empty());
                    saw_started = true;
                }
                ProcessEvent::Chunk(chunk) => match chunk.stream {
                    StreamKind::Stdout => saw_out |= chunk.lines.iter().any(|l| l == "out"),
                    StreamKind::Stderr => saw_err |= chunk.lines.iter().any(|l| l == "err"),
                },
                ProcessEvent::Exited {
                    timestamp,
                    exit_code,
                } => {
                    assert!(timestamp > 0);
                    assert_eq!(exit_code, Some(0));
                    saw_exited = true;
                }
            }
        }

        let status = proc.wait().await?;
        assert!(status.success());
        assert!(saw_started);
        assert!(saw_out);
        assert!(saw_err);
        assert!(saw_exited);
        Ok(())
    }
}
