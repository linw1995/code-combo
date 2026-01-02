use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
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
    ClientMessage, Combo, ComboMetadata, ComboMode, ControlAction, Instruction, MetadataPayload,
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
    Finished {
        exit_code: Option<i32>,
    },
    Cancelled,
    Failed {
        reason: String,
    },
}

#[derive(Debug)]
pub struct PromptRequest {
    pub prompt: String,
    pub schemas: Vec<PromptSchema>,
    pub response_tx: oneshot::Sender<Result<String, String>>,
}

pub type PromptResponder = mpsc::UnboundedSender<PromptRequest>;

#[derive(Default, Debug)]
struct SessionState {
    metadata: Option<MetadataPayload>,
    items: Vec<SessionItem>,
}

#[derive(Debug)]
enum SessionItem {
    Record(RecordedCommand),
    Prompt(String),
}

#[derive(Debug)]
struct RecordedCommand {
    tool_use_id: String,
    command: Vec<String>,
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
    prompt_responder: Option<PromptResponder>,
}

impl StarterCommand {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            envs: Vec::new(),
            discovery: false,
            session_env: None,
            prompt_responder: None,
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

    pub fn prompt_responder(mut self, prompt_responder: PromptResponder) -> Self {
        self.prompt_responder = Some(prompt_responder);
        self
    }

    pub fn execute(self) -> StarterExecution {
        execute_command(
            self.command,
            self.args,
            self.envs,
            self.discovery,
            self.session_env,
            self.prompt_responder,
        )
    }
}

fn parse_combo(command: &str, text: &str) -> Combo {
    let filtered = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("coco record"))
        .collect::<Vec<_>>()
        .join("\n");

    let name = Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("combo")
        .to_string();

    let instructions = if filtered.trim().is_empty() {
        Vec::new()
    } else {
        vec![Instruction::Text(filtered)]
    };

    Combo {
        metadata: ComboMetadata {
            name,
            description: String::new(),
            mode: ComboMode::Unknown,
        },
        instructions,
    }
}

fn record_to_instruction(record: RecordedCommand) -> Instruction {
    record_to_instruction_ref(&record)
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

fn record_to_instruction_ref(record: &RecordedCommand) -> Instruction {
    Instruction::Command {
        input: BashInput::new(record.command.join(" ")),
        output: record_output(record),
    }
}

fn build_combo_from_session(
    command: &str,
    buffer: &str,
    session_state: Option<SessionState>,
) -> Result<Combo, StarterError> {
    if let Some(state) = session_state {
        let metadata = state.metadata.ok_or_else(|| {
            InvalidSnafu {
                reason: "metadata not received from session".to_string(),
            }
            .build()
        })?;
        let instructions = if state.items.is_empty() {
            parse_combo(command, buffer).instructions
        } else {
            state
                .items
                .into_iter()
                .map(|item| match item {
                    SessionItem::Record(record) => record_to_instruction(record),
                    SessionItem::Prompt(prompt) => Instruction::Text(prompt),
                })
                .collect()
        };
        return Ok(Combo {
            metadata: ComboMetadata {
                name: metadata.name,
                description: metadata.description.unwrap_or_default(),
                mode: ComboMode::Unknown,
            },
            instructions,
        });
    }

    Ok(parse_combo(command, buffer))
}

fn execute_command(
    command: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    discovery: bool,
    session_env: Option<SessionEnv>,
    prompt_responder: Option<PromptResponder>,
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
        let prompt_responder = prompt_responder;
        let mut session_server = match _session_env_guard.as_ref() {
            Some(env) => {
                match spawn_session_server(env, discovery, prompt_responder, event_tx.clone()).await
                {
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
                }
            }
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

        let mut buffer = String::new();
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
                            for line in &chunk.lines {
                                buffer.push_str(line);
                                buffer.push('\n');
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
                        let combo = parse_combo(&command, &buffer);
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

                    build_combo_from_session(&command, &buffer, session_state)
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
    prompt_responder: Option<PromptResponder>,
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
            let rv = run_session_server(server, discovery, prompt_responder, event_tx, shutdown_rx)
                .await;
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
    prompt_responder: Option<PromptResponder>,
    event_tx: mpsc::Sender<StarterEvent>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<SessionState, StarterError> {
    let mut state = SessionState::default();
    let mut metadata_seen = false;
    let mut first_message = true;

    loop {
        let accept = tokio::select! {
            _ = &mut shutdown_rx => break,
            accept = server.accept() => accept,
        };
        let mut conn = accept.context(AcceptSessionConnectionSnafu)?;

        let mut current_record_index: Option<usize> = None;

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
                    let record_index = state.items.len();
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
                    state.items.push(SessionItem::Record(RecordedCommand {
                        tool_use_id: tool_use_id.clone(),
                        command: payload.command.clone(),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        exit_code: None,
                    }));
                    current_record_index = Some(record_index);
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
                    let Some(record_index) = current_record_index else {
                        continue;
                    };
                    let Some(SessionItem::Record(record)) = state.items.get_mut(record_index)
                    else {
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
                    let Some(record_index) = current_record_index.take() else {
                        continue;
                    };
                    let Some(SessionItem::Record(record)) = state.items.get_mut(record_index)
                    else {
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
                    let output = record_output(record);
                    let is_error = output.exit_code != 0;
                    let output_value = serde_json::to_value(&output).unwrap_or_else(|_| {
                        json!({
                            "exit_code": output.exit_code,
                            "stdout": output.stdout,
                            "stderr": output.stderr,
                            "timed_out": output.timed_out,
                        })
                    });
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
                        let responder = prompt_responder.as_ref().ok_or_else(|| {
                            InvalidSnafu {
                                reason: "prompt responder is not configured".to_string(),
                            }
                            .build()
                        })?;
                        if !discovery {
                            state
                                .items
                                .push(SessionItem::Prompt(payload.prompt.clone()));
                        }
                        let (response_tx, response_rx) = oneshot::channel();
                        responder
                            .send(PromptRequest {
                                prompt: payload.prompt,
                                schemas: payload.schemas,
                                response_tx,
                            })
                            .map_err(|_| {
                                InvalidSnafu {
                                    reason: "prompt responder is not available".to_string(),
                                }
                                .build()
                            })?;
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
                        state.items.push(SessionItem::Prompt(payload.prompt));
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
    use crate::{ComboMode, Instruction, MetadataPayload, SessionEnv};
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

        let execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();
        let Starter { path, combo } = execution.wait().await?;
        debug!(?path, ?combo, "execute_starter success");
        assert_eq!(path, file_path);
        assert!(combo.is_ok());
        let combo = combo.unwrap();
        assert_eq!(combo.metadata.name, "test");
        assert_eq!(combo.metadata.description, "");
        assert_eq!(combo.metadata.mode, ComboMode::Unknown);
        assert_eq!(combo.instructions.len(), 1);
        assert_eq!(
            combo.instructions.first(),
            Some(&Instruction::Text("Hello world".to_string()))
        );

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

        let execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();
        let Starter { combo, .. } = execution.wait().await?;
        let combo = combo?;
        assert_eq!(combo.metadata.name, "record");
        assert_eq!(combo.instructions.len(), 1);
        debug!(?combo, "print combo");

        let Some(Instruction::Command { input, output }) = combo.instructions.first() else {
            panic!("expected first instruction to be Instruction::Command");
        };
        assert!(
            input.command.starts_with("bash -c "),
            "unexpected command: {}",
            input.command
        );
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

        let execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .execute();
        let Starter { combo, .. } = execution.wait().await?;
        let combo = combo?;
        assert_eq!(combo.metadata.name, "ask");
        assert_eq!(combo.instructions.len(), 2);

        assert_eq!(
            combo.instructions.first(),
            Some(&Instruction::Text("Please do the thing".to_string()))
        );

        let Some(Instruction::Command { input, output }) = combo.instructions.get(1) else {
            panic!("expected second instruction to be Instruction::Command");
        };
        assert!(
            input.command.starts_with("bash -c "),
            "unexpected command: {}",
            input.command
        );
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

        let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<PromptRequest>();
        tokio::spawn(async move {
            while let Some(request) = prompt_rx.recv().await {
                let _ = request.response_tx.send(Ok("ok".to_string()));
            }
        });

        let execution = StarterCommand::new(&file_path)
            .session_env(session_env)
            .prompt_responder(prompt_tx)
            .execute();
        let Starter { combo, .. } = execution.wait().await?;
        let combo = combo?;
        assert_eq!(combo.metadata.name, "ask_reply");
        assert_eq!(combo.instructions.len(), 1);
        assert_eq!(
            combo.instructions.first(),
            Some(&Instruction::Text("Please do the thing".to_string()))
        );

        Ok(())
    }

    #[tokio::test]
    async fn discovery_server_interrupts_record() -> Result<(), Box<dyn std::error::Error>> {
        let session_env = session_env_with_coco();
        let (event_tx, _event_rx) = mpsc::channel(16);
        let server = spawn_session_server(&session_env, true, None, event_tx).await?;
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
        let server = spawn_session_server(&session_env, true, None, event_tx).await?;
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

        let execution = StarterCommand::new(&file_path)
            .env("GREETING", "Hello from envs")
            .session_env(session_env)
            .execute();
        let Starter { combo, .. } = execution.wait().await?;
        assert!(combo.is_ok());
        let instructions = combo.unwrap().instructions;
        assert!(
            instructions
                .iter()
                .any(|inst| inst == &Instruction::Text("Hello from envs".to_string()))
        );

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
