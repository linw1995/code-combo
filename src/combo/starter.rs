use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures_core::Stream;
use futures_util::StreamExt;
use snafu::prelude::*;
use time::OffsetDateTime;
use tokio::{
    sync::{mpsc, oneshot},
    task::{self, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::tools::{BASH_TOOL_NAME, BashInput, BashOutput, Final};
use crate::{
    ClientMessage, Combo, ComboMetadata, ComboMode, ControlAction, MetadataPayload,
    MetadataResponse, PromptSchema, RecordControl, RecordEndPayload, ServerMessage, SessionEnv,
    SessionSocketServer, StreamKind, ToolUse,
    exec::{ChunkConfig, ExecCommand, OutputChunk, ProcessEvent},
};
use serde_json::json;

#[derive(Debug, Clone, Snafu)]
pub enum StarterError {
    #[snafu(display("Starter timeout after {seconds}s"))]
    Timeout { seconds: usize },
    #[snafu(display("Combo file is not excutable"))]
    NotExcutable,
    #[snafu(display("Invalid combo: {reason}"))]
    Invalid { reason: String },
    #[snafu(display("Starter execution was cancelled"))]
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Starter {
    pub path: String,
    pub combo: Result<Combo, StarterError>,
}

#[derive(Debug, Clone)]
pub enum StarterEvent {
    Started {
        command: String,
        args: Vec<String>,
    },
    Output {
        chunk: OutputChunk,
    },
    RecordStart {
        tool_use: ToolUse,
    },
    RecordOutput {
        tool_use_id: String,
        chunk: OutputChunk,
    },
    RecordEnd {
        tool_use_id: String,
        is_error: bool,
        output: Final,
    },
    Prompt {
        prompt: String,
    },
    PromptRequest {
        prompt: String,
        schemas: Vec<PromptSchema>,
        responder: PromptResponseSender,
    },
    Finished {
        exit_code: Option<i32>,
    },
    Cancelled,
    Failed {
        reason: String,
    },
}

type PromptResponseSenderState = Arc<Mutex<Option<oneshot::Sender<Result<String, String>>>>>;

#[derive(Clone)]
pub struct PromptResponseSender(PromptResponseSenderState);

impl PromptResponseSender {
    pub fn new(sender: oneshot::Sender<Result<String, String>>) -> Self {
        Self(Arc::new(Mutex::new(Some(sender))))
    }

    pub fn send(&self, response: Result<String, String>) -> Result<(), String> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| "prompt response sender lock poisoned".to_string())?;
        let Some(sender) = guard.take() else {
            return Err(
                "prompt response sender already used; it can only be used once".to_string(),
            );
        };
        sender
            .send(response)
            .map_err(|_| "prompt response receiver dropped".to_string())
    }
}

impl std::fmt::Debug for PromptResponseSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PromptResponseSender")
    }
}

#[derive(Default, Debug)]
struct SessionState {
    metadata: Option<MetadataPayload>,
}

#[derive(Debug)]
struct RecordedCommand {
    tool_use_id: String,
    stdout: Vec<String>,
    stderr: Vec<String>,
    exit_code: Option<i32>,
}

#[derive(Debug)]
pub struct StarterExecution {
    join_handle: JoinHandle<Starter>,
    cancel_tx: Option<oneshot::Sender<()>>,
    events_rx: mpsc::Receiver<StarterEvent>,
}

impl Stream for StarterExecution {
    type Item = StarterEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.events_rx.poll_recv(cx)
    }
}

impl StarterExecution {
    pub fn cancel(&mut self) {
        if let Some(cancel_tx) = self.cancel_tx.take() {
            cancel_tx.send(()).ok();
        }
    }

    pub async fn wait(self) -> Result<Starter, tokio::task::JoinError> {
        self.join_handle.await
    }

    pub async fn consume_with_cancel<F>(
        mut self,
        cancel_token: CancellationToken,
        mut on_event: F,
    ) -> Result<Starter, tokio::task::JoinError>
    where
        F: FnMut(StarterEvent),
    {
        let mut cancelled = false;
        loop {
            tokio::select! {
                _ = cancel_token.cancelled(), if !cancelled => {
                    cancelled = true;
                    self.cancel();
                }
                event = self.next() => {
                    let Some(event) = event else {
                        break;
                    };
                    on_event(event);
                }
            }
        }
        self.wait().await
    }
}

#[derive(Debug)]
pub struct StarterCommand {
    command: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    discovery: bool,
    session_env: Option<SessionEnv>,
}

impl StarterCommand {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            envs: Vec::new(),
            discovery: false,
            session_env: None,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
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

    pub fn discovery(mut self, discovery: bool) -> Self {
        self.discovery = discovery;
        self
    }

    pub fn session_env(mut self, session_env: SessionEnv) -> Self {
        self.session_env = Some(session_env);
        self
    }

    pub fn execute(self) -> StarterExecution {
        execute_command(
            self.command,
            self.args,
            self.envs,
            self.discovery,
            self.session_env,
        )
    }
}

fn parse_combo(command: &str) -> Combo {
    let name = Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("combo")
        .to_string();

    Combo {
        metadata: ComboMetadata {
            name,
            description: String::new(),
            mode: ComboMode::Unknown,
        },
    }
}

fn record_output(record: &RecordedCommand) -> BashOutput {
    let stdout = if record.stdout.is_empty() {
        String::new()
    } else {
        record.stdout.join("\n")
    };
    let stderr = if record.stderr.is_empty() {
        String::new()
    } else {
        record.stderr.join("\n")
    };
    let exit_code = record
        .exit_code
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(255);

    BashOutput {
        exit_code,
        stdout,
        stderr,
        timed_out: false,
    }
}

fn build_combo_from_session(
    command: &str,
    session_state: Option<SessionState>,
) -> Result<Combo, StarterError> {
    if let Some(state) = session_state {
        let metadata = state.metadata.ok_or_else(|| {
            InvalidSnafu {
                reason: "metadata not received from session".to_string(),
            }
            .build()
        })?;
        return Ok(Combo {
            metadata: ComboMetadata {
                name: metadata.name,
                description: metadata.description.unwrap_or_default(),
                mode: ComboMode::Unknown,
            },
        });
    }

    Ok(parse_combo(command))
}

fn execute_command(
    command: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    discovery: bool,
    session_env: Option<SessionEnv>,
) -> StarterExecution {
    let session_env_envs = session_env
        .as_ref()
        .map(|env| {
            env.envs().into_iter().map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.to_string_lossy().to_string(),
                )
            })
        })
        .into_iter()
        .flatten();
    let merged_envs = envs.into_iter().chain(session_env_envs).collect::<Vec<_>>();
    let (event_tx, events_rx) = mpsc::channel(16);
    let (cancel_tx, cancel_rx) = oneshot::channel();

    let join_handle = task::spawn(async move {
        let _session_env_guard = session_env;
        let mut session_server = match _session_env_guard.as_ref() {
            Some(env) => match spawn_session_server(env, discovery, event_tx.clone()).await {
                Ok(task) => Some(task),
                Err(error) => {
                    event_tx
                        .send(StarterEvent::Failed {
                            reason: error.to_string(),
                        })
                        .await
                        .ok();
                    return Starter {
                        path: command,
                        combo: Err(error),
                    };
                }
            },
            None => {
                let error = InvalidSnafu {
                    reason: "session env is required for starter execution".to_string(),
                }
                .build();
                event_tx
                    .send(StarterEvent::Failed {
                        reason: error.to_string(),
                    })
                    .await
                    .ok();
                return Starter {
                    path: command,
                    combo: Err(error),
                };
            }
        };
        let event_tx = event_tx;
        let mut cancel_rx = cancel_rx;

        let argv = std::iter::once(command.clone())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();
        let mut proc = match ExecCommand::from_argv(argv)
            .stdin(Stdio::piped())
            .envs(merged_envs)
            .spawn_chunked(ChunkConfig::default())
        {
            Ok(proc) => proc,
            Err(err) => {
                let error = match err.kind() {
                    ErrorKind::PermissionDenied => NotExcutableSnafu.build(),
                    _ => InvalidSnafu {
                        reason: format!("excuting error: {err}"),
                    }
                    .build(),
                };
                event_tx
                    .send(StarterEvent::Failed {
                        reason: error.to_string(),
                    })
                    .await
                    .ok();
                return Starter {
                    path: command,
                    combo: Err(error),
                };
            }
        };

        event_tx
            .send(StarterEvent::Started {
                command: command.clone(),
                args: args.clone(),
            })
            .await
            .ok();

        let mut cancelled = false;
        let mut exit_code: Option<i32> = None;

        loop {
            tokio::select! {
                _ = &mut cancel_rx, if !cancelled => {
                    cancelled = true;
                    proc.killer.kill();
                    info!(?command, "execution cancelled");
                }
                event = proc.events.next() => {
                    let Some(event) = event else { break };
                    match event {
                        ProcessEvent::Started { .. } => {}
                        ProcessEvent::Chunk(chunk) => {
                            if cancelled {
                                continue;
                            }
                            event_tx.send(StarterEvent::Output { chunk }).await.ok();
                        }
                        ProcessEvent::Exited { exit_code: code, .. } => {
                            exit_code = code;
                            break;
                        }
                    }
                }
            }
        }

        debug!("wait finished");
        let wait_result = proc.wait().await;
        if cancelled {
            event_tx.send(StarterEvent::Cancelled).await.ok();
            if let Some(task) = session_server.take() {
                task.abort();
            }
            return Starter {
                path: command,
                combo: Err(StarterError::Cancelled),
            };
        }

        let combo = match wait_result {
            Ok(status) => {
                event_tx
                    .send(StarterEvent::Finished {
                        exit_code: exit_code.or_else(|| status.code()),
                    })
                    .await
                    .ok();
                if discovery && !status.success() {
                    if let Some(task) = session_server.take() {
                        task.abort();
                    }
                    Err(InvalidSnafu {
                        reason: format!(
                            "starter exited with status {:?} during discovery",
                            status.code()
                        ),
                    }
                    .build())
                } else {
                    if discovery {
                        if let Some(task) = session_server.take() {
                            task.abort();
                        }
                        let combo = parse_combo(&command);
                        return Starter {
                            path: command,
                            combo: Ok(combo),
                        };
                    }
                    let session_state = if let Some(mut task) = session_server.take() {
                        task.shutdown();
                        match task.handle.await {
                            Ok(Ok(state)) => Some(state),
                            Ok(Err(err)) => {
                                event_tx
                                    .send(StarterEvent::Failed {
                                        reason: err.to_string(),
                                    })
                                    .await
                                    .ok();
                                return Starter {
                                    path: command,
                                    combo: Err(err),
                                };
                            }
                            Err(err) => {
                                let error = InvalidSnafu {
                                    reason: format!("session server join error: {err}"),
                                }
                                .build();
                                event_tx
                                    .send(StarterEvent::Failed {
                                        reason: error.to_string(),
                                    })
                                    .await
                                    .ok();
                                return Starter {
                                    path: command,
                                    combo: Err(error),
                                };
                            }
                        }
                    } else {
                        None
                    };

                    build_combo_from_session(&command, session_state)
                }
            }
            Err(err) => {
                let error = InvalidSnafu {
                    reason: format!("excuting error: {err}"),
                }
                .build();
                if let Some(task) = session_server.take() {
                    task.abort();
                }
                event_tx
                    .send(StarterEvent::Failed {
                        reason: error.to_string(),
                    })
                    .await
                    .ok();
                Err(error)
            }
        };

        Starter {
            path: command,
            combo,
        }
    });

    StarterExecution {
        join_handle,
        cancel_tx: Some(cancel_tx),
        events_rx,
    }
}

struct SessionServerTask {
    handle: JoinHandle<Result<SessionState, StarterError>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Snafu)]
enum SessionServerSetupError {
    #[snafu(display("failed to clean old session socket {path:?}: {source}"))]
    RemoveOldSessionSocket {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to bind session socket {path:?}: {source}"))]
    BindSessionSocket {
        path: PathBuf,
        source: crate::SessionServerError,
    },

    #[snafu(display("failed to accept session connection: {source}"))]
    AcceptSessionConnection { source: crate::SessionServerError },
}

impl From<SessionServerSetupError> for StarterError {
    fn from(err: SessionServerSetupError) -> Self {
        Self::Invalid {
            reason: err.to_string(),
        }
    }
}

impl SessionServerTask {
    fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            tx.send(()).ok();
        }
    }

    fn abort(&self) {
        self.handle.abort();
    }
}

async fn spawn_session_server(
    env: &SessionEnv,
    discovery: bool,
    event_tx: mpsc::Sender<StarterEvent>,
) -> Result<SessionServerTask, StarterError> {
    let path = env.socket_path().to_path_buf();
    if path.exists() {
        std::fs::remove_file(&path).context(RemoveOldSessionSocketSnafu { path: path.clone() })?;
    }
    let server = SessionSocketServer::bind(&path)
        .await
        .context(BindSessionSocketSnafu { path: path.clone() })?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    Ok(SessionServerTask {
        handle: tokio::spawn(async move {
            let rv = run_session_server(server, discovery, event_tx, shutdown_rx).await;
            match &rv {
                Ok(state) => info!(?state, "succeed to run session server"),
                Err(err) => warn!(?err, "failed to run session server"),
            };
            rv
        }),
        shutdown_tx: Some(shutdown_tx),
    })
}

async fn run_session_server(
    server: SessionSocketServer,
    discovery: bool,
    event_tx: mpsc::Sender<StarterEvent>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<SessionState, StarterError> {
    let mut state = SessionState::default();
    let mut metadata_seen = false;
    let mut first_message = true;
    let mut event_index: usize = 0;

    loop {
        let accept = tokio::select! {
            _ = &mut shutdown_rx => break,
            accept = server.accept() => accept,
        };
        let mut conn = accept.context(AcceptSessionConnectionSnafu)?;

        let mut current_record: Option<RecordedCommand> = None;

        loop {
            let message = tokio::select! {
                _ = &mut shutdown_rx => {
                    if !metadata_seen {
                        return Err(InvalidSnafu {
                            reason: "metadata not received from session".to_string(),
                        }
                        .build());
                    }
                    return Ok(state);
                },
                message = conn.read_client_message() => message,
            };

            match message {
                Ok(ClientMessage::Metadata(payload)) => {
                    if !first_message || metadata_seen {
                        let _ = conn
                            .send_server_message(&ServerMessage::RecordControl(RecordControl {
                                action: ControlAction::Interrupt,
                            }))
                            .await;
                        return Err(InvalidSnafu {
                            reason: "metadata must be the first and only metadata message"
                                .to_string(),
                        }
                        .build());
                    }
                    metadata_seen = true;
                    state.metadata = Some(payload);
                    let _ = conn
                        .send_server_message(&ServerMessage::Metadata(MetadataResponse {
                            discovery,
                        }))
                        .await;
                    first_message = false;
                }
                Ok(ClientMessage::RecordStart(payload)) => {
                    if discovery || !metadata_seen {
                        let _ = conn
                            .send_server_message(&ServerMessage::RecordControl(RecordControl {
                                action: ControlAction::Interrupt,
                            }))
                            .await;
                        return Err(InvalidSnafu {
                            reason:
                                "record commands are not allowed in discovery or before metadata"
                                    .to_string(),
                        }
                        .build());
                    }
                    let record_index = event_index;
                    event_index = event_index.saturating_add(1);
                    let name = state
                        .metadata
                        .as_ref()
                        .map(|metadata| metadata.name.as_str())
                        .unwrap_or("combo");
                    let tool_use_id = format!("combo_record_{name}_{record_index}");
                    let input = BashInput::new(payload.command.join(" "));
                    let tool_use = ToolUse {
                        id: tool_use_id.clone(),
                        name: BASH_TOOL_NAME.to_string(),
                        input: serde_json::to_value(&input).unwrap_or_else(|_| {
                            json!({
                                "command": input.command,
                                "timeout": input.timeout,
                            })
                        }),
                    };
                    current_record = Some(RecordedCommand {
                        tool_use_id: tool_use_id.clone(),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        exit_code: None,
                    });
                    let _ = conn
                        .send_server_message(&ServerMessage::RecordControl(RecordControl {
                            action: ControlAction::Allow,
                        }))
                        .await;
                    event_tx
                        .send(StarterEvent::RecordStart { tool_use })
                        .await
                        .ok();
                    first_message = false;
                }
                Ok(ClientMessage::RecordChunk(chunk)) => {
                    if discovery {
                        let _ = conn
                            .send_server_message(&ServerMessage::RecordControl(RecordControl {
                                action: ControlAction::Interrupt,
                            }))
                            .await;
                        return Err(InvalidSnafu {
                            reason: "record chunk is not allowed during discovery".to_string(),
                        }
                        .build());
                    }
                    let stream = chunk.stream;
                    let lines = chunk.lines;
                    let Some(record) = current_record.as_mut() else {
                        continue;
                    };
                    let tool_use_id = record.tool_use_id.clone();

                    match stream {
                        StreamKind::Stdout => record.stdout.extend(lines.clone()),
                        StreamKind::Stderr => record.stderr.extend(lines.clone()),
                    }
                    event_tx
                        .send(StarterEvent::RecordOutput {
                            tool_use_id,
                            chunk: OutputChunk {
                                timestamp: OffsetDateTime::now_utc().unix_timestamp(),
                                stream,
                                lines,
                            },
                        })
                        .await
                        .ok();
                }
                Ok(ClientMessage::RecordEnd(RecordEndPayload {
                    exit_code,
                    stdout,
                    stderr,
                    ..
                })) => {
                    if discovery {
                        let _ = conn
                            .send_server_message(&ServerMessage::RecordControl(RecordControl {
                                action: ControlAction::Interrupt,
                            }))
                            .await;
                        return Err(InvalidSnafu {
                            reason: "record end is not allowed during discovery".to_string(),
                        }
                        .build());
                    }
                    let Some(mut record) = current_record.take() else {
                        continue;
                    };

                    if let Some(stdout) = stdout {
                        record.stdout.push(stdout);
                    }
                    if let Some(stderr) = stderr {
                        record.stderr.push(stderr);
                    }
                    record.exit_code = exit_code;
                    let tool_use_id = record.tool_use_id.clone();
                    let output = record_output(&record);
                    let is_error = output.exit_code != 0;
                    let output_value =
                        serde_json::to_value(&output).expect("failed to encode record output");
                    event_tx
                        .send(StarterEvent::RecordEnd {
                            tool_use_id,
                            is_error,
                            output: Final::from(output_value),
                        })
                        .await
                        .ok();
                }
                Ok(ClientMessage::Prompt(payload)) => {
                    if !metadata_seen {
                        return Err(InvalidSnafu {
                            reason: "prompt is not allowed before metadata".to_string(),
                        }
                        .build());
                    }
                    if payload.reply {
                        if discovery {
                            return Err(InvalidSnafu {
                                reason: "prompt reply is not allowed during discovery".to_string(),
                            }
                            .build());
                        }
                        if payload.schemas.is_empty() {
                            return Err(InvalidSnafu {
                                reason: "prompt reply requires schemas".to_string(),
                            }
                            .build());
                        }
                        let (response_tx, response_rx) = oneshot::channel();
                        let responder = PromptResponseSender::new(response_tx);
                        event_tx
                            .send(StarterEvent::PromptRequest {
                                prompt: payload.prompt,
                                schemas: payload.schemas,
                                responder,
                            })
                            .await
                            .map_err(|_| {
                                InvalidSnafu {
                                    reason: "prompt responder is not available".to_string(),
                                }
                                .build()
                            })?;
                        event_index = event_index.saturating_add(1);
                        let response = response_rx.await.map_err(|_| {
                            InvalidSnafu {
                                reason: "prompt responder dropped response".to_string(),
                            }
                            .build()
                        })?;
                        let response = response.map_err(|err| {
                            InvalidSnafu {
                                reason: format!("prompt responder failed: {err}"),
                            }
                            .build()
                        })?;
                        conn.send_server_message(&ServerMessage::PromptResponse(response))
                            .await
                            .map_err(|err| {
                                InvalidSnafu {
                                    reason: format!("failed to send prompt response: {err}"),
                                }
                                .build()
                            })?;
                    } else if !discovery {
                        event_tx
                            .send(StarterEvent::Prompt {
                                prompt: payload.prompt,
                            })
                            .await
                            .ok();
                        event_index = event_index.saturating_add(1);
                    }
                    first_message = false;
                }
                Err(err) => {
                    if !metadata_seen {
                        return Err(InvalidSnafu {
                            reason: format!("failed before receiving metadata: {err}"),
                        }
                        .build());
                    }
                    break;
                }
            }
        }
    }

    if !metadata_seen {
        return Err(InvalidSnafu {
            reason: "metadata not received from session".to_string(),
        }
        .build());
    }

    Ok(state)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::{path::PathBuf, sync::OnceLock};

    use indoc::{formatdoc, indoc};
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use crate::combo::{RecordStartPayload, SessionSocketClient};
    use crate::{ComboMode, MetadataPayload, SessionEnv};
    use tokio::time::Duration;

    static COCO_BIN_PATH: OnceLock<PathBuf> = OnceLock::new();

    fn coco_binary() -> PathBuf {
        COCO_BIN_PATH
            .get_or_init(|| {
                if let Ok(path) = std::env::var("COCO_TEST_BIN") {
                    return PathBuf::from(path);
                }
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("debug")
                    .join("coco")
            })
            .clone()
    }

    fn session_env_with_coco() -> SessionEnv {
        let path = coco_binary();
        assert!(
            path.exists(),
            indoc! {"
                coco binary not found at {:?};
                build `cargo build -p code-combo --bin coco` first
                or set COCO_TEST_BIN
            "},
            path
        );
        SessionEnv::builder()
            .binary_path(&path)
            .command_name("coco")
            .build()
            .expect("build session env")
    }

    use super::*;

    fn find_bash() -> String {
        // why not `/usr/bin/env`? Because it is not working in the Nix build sandbox
        which::which("bash")
            .expect("failed to find bash")
            .to_string_lossy()
            .to_string()
    }

    async fn create_temp_combo(
        name: &str,
        code: &str,
    ) -> Result<(TempDir, String), Box<dyn std::error::Error>> {
        // Create a temporary directory and test file
        let temp_dir = tempfile::tempdir()?;
        let file_path = temp_dir.path().join(name);

        debug!(?file_path, ?code, "create temp file");
        std::fs::write(&file_path, code)?;

        // Get current permissions
        let metadata = tokio::fs::metadata(&file_path).await?;
        let mut permissions = metadata.permissions();

        // Set the executable bit (octal 0o755 for owner read/write/execute, group read/execute, others read/execute)
        // For Windows, this might not have the same effect as on Unix-like systems,
        // as Windows handles executable status differently.
        permissions.set_mode(0o755);

        // Apply the new permissions
        tokio::fs::set_permissions(&file_path, permissions).await?;

        Ok((temp_dir, file_path.to_string_lossy().to_string()))
    }

    fn bash_output_from_final(output: &Final) -> BashOutput {
        match output {
            Final::Json(value) => {
                serde_json::from_value(value.clone()).expect("invalid bash output payload")
            }
            Final::Message(message) => {
                panic!("unexpected message output: {message}");
            }
        }
    }

    #[tokio::test]
    async fn execute_starter_emits_failed_reason_without_session_env()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_guard, file_path) =
            create_temp_combo("no_session.sh", "#!/bin/sh\n\nexit 0\n").await?;

        let mut execution = StarterCommand::new(file_path).execute();
        let event = execution.next().await;
        let Some(StarterEvent::Failed { reason }) = event else {
            panic!("expected StarterEvent::Failed, got {event:?}");
        };
        assert!(
            reason.contains("session env is required"),
            "unexpected reason: {reason}"
        );

        let Starter { combo, .. } = execution.wait().await?;
        assert!(matches!(combo, Err(StarterError::Invalid { .. })));

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_in_discovery() -> Result<(), Box<dyn std::error::Error>> {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "commit.sh",
            formatdoc! {r#"
            #!{bash}

            coco metadata name=commit || exit 0

            echo "Instruction line 1"
            echo "Instruction line 2"
            "#}
            .as_str(),
        )
        .await?;

        let execution = StarterCommand::new(&file_path)
            .discovery(true)
            .session_env(session_env)
            .execute();
        let Starter { path, combo } = execution.wait().await?;
        debug!(?path, ?combo, "execute_starter success");
        assert_eq!(path, file_path);
        assert!(combo.is_ok());
        let combo = combo.unwrap();
        assert_eq!(combo.metadata.name, "commit");

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_continue() -> Result<(), Box<dyn std::error::Error>> {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "test.sh",
            formatdoc! {r#"
            #!{bash}

            coco metadata name=test || exit 0

            echo "Hello world"
            "#}
            .as_str(),
        )
        .await?;

        let mut execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();
        let mut saw_output = false;
        let mut saw_prompt = false;
        while let Some(event) = execution.next().await {
            match event {
                StarterEvent::Output { chunk } => {
                    if chunk.lines.iter().any(|line| line.contains("Hello world")) {
                        saw_output = true;
                    }
                }
                StarterEvent::Prompt { .. } | StarterEvent::PromptRequest { .. } => {
                    saw_prompt = true;
                }
                _ => {}
            }
        }
        let Starter { path, combo } = execution.wait().await?;
        debug!(?path, ?combo, "execute_starter success");
        assert_eq!(path, file_path);
        assert!(combo.is_ok());
        let combo = combo.unwrap();
        assert_eq!(combo.metadata.name, "test");
        assert_eq!(combo.metadata.description, "");
        assert_eq!(combo.metadata.mode, ComboMode::Unknown);
        assert!(saw_output, "expected output event to include Hello world");
        assert!(!saw_prompt, "unexpected prompt event in output-only combo");

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_records_coco_record_output() -> Result<(), Box<dyn std::error::Error>>
    {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "record.sh",
            formatdoc! {r#"
            #!{bash}

            coco metadata name=record || exit 0

            coco record "echo out; echo err 1>&2"
            "#}
            .as_str(),
        )
        .await?;

        let mut execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();
        let mut saw_record_start = false;
        let mut saw_record_output = false;
        let mut record_output: Option<BashOutput> = None;
        while let Some(event) = execution.next().await {
            match event {
                StarterEvent::RecordStart { tool_use } => {
                    saw_record_start = true;
                    assert_eq!(tool_use.name, BASH_TOOL_NAME);
                }
                StarterEvent::RecordOutput { chunk, .. } => {
                    if chunk
                        .lines
                        .iter()
                        .any(|line| line.contains("out") || line.contains("err"))
                    {
                        saw_record_output = true;
                    }
                }
                StarterEvent::RecordEnd { output, .. } => {
                    record_output = Some(bash_output_from_final(&output));
                }
                _ => {}
            }
        }
        let Starter { combo, .. } = execution.wait().await?;
        let combo = combo?;
        assert_eq!(combo.metadata.name, "record");

        assert!(saw_record_start, "expected record start event");
        assert!(saw_record_output, "expected record output event");
        let output = record_output.expect("expected record end output");
        assert!(
            output.stdout.contains("out"),
            "unexpected stdout: {}",
            output.stdout
        );
        assert!(
            output.stderr.contains("err"),
            "unexpected stderr: {}",
            output.stderr
        );
        assert_eq!(output.exit_code, 0, "unexpected exit_code");
        assert!(!output.timed_out, "unexpected timed_out");

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_records_coco_ask_prompt() -> Result<(), Box<dyn std::error::Error>> {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "ask.sh",
            formatdoc! {r#"
            #!{bash}

            coco metadata name=ask || exit 0

            coco ask "Please do the thing"
            coco record "echo out"
            "#}
            .as_str(),
        )
        .await?;

        let mut execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();
        let mut prompt_text = None;
        let mut record_output: Option<BashOutput> = None;
        while let Some(event) = execution.next().await {
            match event {
                StarterEvent::Prompt { prompt } => {
                    prompt_text = Some(prompt);
                }
                StarterEvent::RecordEnd { output, .. } => {
                    record_output = Some(bash_output_from_final(&output));
                }
                _ => {}
            }
        }
        let Starter { combo, .. } = execution.wait().await?;
        let combo = combo?;
        assert_eq!(combo.metadata.name, "ask");

        assert_eq!(prompt_text.as_deref(), Some("Please do the thing"));
        let output = record_output.expect("expected record end output");
        assert!(
            output.stdout.contains("out"),
            "unexpected stdout: {}",
            output.stdout
        );

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_records_coco_ask_prompt_with_reply()
    -> Result<(), Box<dyn std::error::Error>> {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "ask_reply.sh",
            formatdoc! {r#"
            #!{bash}

            coco metadata name=ask_reply || exit 0

            coco ask --reply --schemas response:message "Please do the thing"
            "#}
            .as_str(),
        )
        .await?;

        let mut execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();
        let mut prompt_text = None;
        while let Some(event) = execution.next().await {
            if let StarterEvent::PromptRequest {
                prompt, responder, ..
            } = event
            {
                prompt_text = Some(prompt.clone());
                let _ = responder.send(Ok("ok".to_string()));
            }
        }
        let Starter { combo, .. } = execution.wait().await?;
        let combo = combo?;
        assert_eq!(combo.metadata.name, "ask_reply");
        assert_eq!(prompt_text.as_deref(), Some("Please do the thing"));

        Ok(())
    }

    #[tokio::test]
    async fn discovery_server_interrupts_record() -> Result<(), Box<dyn std::error::Error>> {
        let session_env = session_env_with_coco();
        let (event_tx, _event_rx) = mpsc::channel(16);
        let server = spawn_session_server(&session_env, true, event_tx).await?;
        let client = SessionSocketClient::connect(session_env.socket_path()).await?;

        let _ = client
            .send_metadata_wait_response(MetadataPayload {
                name: "interrupt".into(),
                description: None,
                model: None,
                tools: None,
            })
            .await?;

        let mut record = client
            .begin_record(RecordStartPayload {
                command: vec!["echo".into()],
                started_at: 0,
            })
            .await?;

        let result = record.wait_for_allow().await;
        assert!(matches!(
            result,
            Err(crate::SessionClientError::Interrupted)
        ));

        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn discovery_server_accepts_metadata_once() -> Result<(), Box<dyn std::error::Error>> {
        let session_env = session_env_with_coco();
        let (event_tx, _event_rx) = mpsc::channel(16);
        let server = spawn_session_server(&session_env, true, event_tx).await?;
        let client = SessionSocketClient::connect(session_env.socket_path()).await?;

        let response = client
            .send_metadata_wait_response(MetadataPayload {
                name: "meta".into(),
                description: None,
                model: None,
                tools: None,
            })
            .await?;
        assert!(response.discovery);

        let second = client
            .send_metadata_wait_response(MetadataPayload {
                name: "again".into(),
                description: None,
                model: None,
                tools: None,
            })
            .await;
        assert!(matches!(
            second,
            Err(crate::SessionClientError::UnexpectedServerMessage { .. })
        ));

        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_discovery_fails_on_non_zero_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "fail.sh",
            formatdoc! {r#"
            #!{bash}

            coco metadata name=fail
            "#}
            .as_str(),
        )
        .await?;

        let execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .discovery(true)
            .execute();
        let Starter { combo, .. } = execution.wait().await?;
        assert!(matches!(combo, Err(StarterError::Invalid { .. })));

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_with_envs() -> Result<(), Box<dyn std::error::Error>> {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "env.sh",
            formatdoc! {r#"
            #!{bash}

            coco metadata name=env || exit 0

            echo "$GREETING"
            "#}
            .as_str(),
        )
        .await?;

        let mut execution = StarterCommand::new(&file_path)
            .env("GREETING", "Hello from envs")
            .session_env(session_env)
            .execute();
        let mut saw_env_output = false;
        while let Some(event) = execution.next().await {
            if let StarterEvent::Output { chunk } = event
                && chunk
                    .lines
                    .iter()
                    .any(|line| line.contains("Hello from envs"))
            {
                saw_env_output = true;
            }
        }
        let Starter { combo, .. } = execution.wait().await?;
        assert!(combo.is_ok());
        assert!(saw_env_output, "expected env output to be streamed");

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_cancel() -> Result<(), Box<dyn std::error::Error>> {
        let bash = find_bash();
        let session_env = session_env_with_coco();
        let (_guard, file_path) = create_temp_combo(
            "cancel.sh",
            formatdoc! {r#"
            #!{bash}

            set -e

            coco metadata name=cancel || exit 0

            while true; do
              echo "waiting"
              sleep 1
            done
            "#}
            .as_str(),
        )
        .await?;

        let mut execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();

        tokio::time::sleep(Duration::from_millis(200)).await;

        execution.cancel();

        while let Some(event) = execution.next().await {
            debug!(?event, "print next event");
        }

        let Starter { combo, .. } = execution.wait().await?;
        assert!(matches!(combo, Err(StarterError::Cancelled)));

        Ok(())
    }
}
