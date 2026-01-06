use std::{path::Path, path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Mutex,
};
use tracing::{debug, warn};

use crate::{MCP_SOCKET_ENV, McpRequest, McpResponse, exec::StreamKind};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientMessage {
    Metadata(MetadataPayload),
    RecordStart(RecordStartPayload),
    RecordChunk(RecordChunkPayload),
    RecordEnd(RecordEndPayload),
    Prompt(PromptPayload),
    Mcp(McpRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerMessage {
    RecordControl(RecordControl),
    PromptResponse(String),
    Metadata(MetadataResponse),
    Mcp(McpResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordControl {
    pub action: ControlAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Allow,
    Interrupt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetadataPayload {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetadataResponse {
    pub discovery: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordStartPayload {
    pub command: Vec<String>,
    pub started_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordChunkPayload {
    pub stream: StreamKind,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordEndPayload {
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    pub ended_at: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptPayload {
    pub prompt: String,
    #[serde(default)]
    pub reply: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schemas: Vec<PromptSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptSchema {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum SessionClientError {
    #[snafu(display("failed to serialize session message"))]
    Serialize { source: serde_json::Error },

    #[snafu(display("failed to deserialize session message"))]
    Deserialize { source: serde_json::Error },

    #[snafu(display("failed to connect to session socket {socket_path:?}"))]
    Connect {
        socket_path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to send message to session socket {socket_path:?}"))]
    Send {
        socket_path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to read message from session socket {socket_path:?}"))]
    Receive {
        socket_path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("unexpected server message: {message:?}"))]
    UnexpectedServerMessage { message: ServerMessage },

    #[snafu(display("record execution was interrupted by server"))]
    Interrupted,
}

pub type ClientResult<T, E = SessionClientError> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct SessionSocketClient {
    socket_path: PathBuf,
    reader: Arc<Mutex<BufReader<tokio::net::unix::OwnedReadHalf>>>,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
}

impl SessionSocketClient {
    pub async fn connect(socket_path: impl Into<PathBuf>) -> ClientResult<Self> {
        let socket_path = socket_path.into();
        let stream = UnixStream::connect(&socket_path).await.context(
            session_client_error::ConnectSnafu {
                socket_path: socket_path.clone(),
            },
        )?;
        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            socket_path,
            reader: Arc::new(Mutex::new(BufReader::new(read_half))),
            writer: Arc::new(Mutex::new(write_half)),
        })
    }

    pub async fn from_env() -> ClientResult<Option<Self>> {
        Self::from_env_key("COCO_SESSION_SOCK").await
    }

    pub async fn from_mcp_env() -> ClientResult<Option<Self>> {
        Self::from_env_key(MCP_SOCKET_ENV).await
    }

    async fn from_env_key(key: &str) -> ClientResult<Option<Self>> {
        match std::env::var_os(key) {
            Some(path) => Ok(Some(Self::connect(path).await?)),
            None => Ok(None),
        }
    }

    pub async fn send_metadata(&self, payload: MetadataPayload) -> ClientResult<()> {
        self.send_message(&ClientMessage::Metadata(payload)).await
    }

    pub async fn send_metadata_wait_response(
        &self,
        payload: MetadataPayload,
    ) -> ClientResult<MetadataResponse> {
        self.send_message(&ClientMessage::Metadata(payload)).await?;
        match self.read_server_message().await? {
            ServerMessage::Metadata(response) => Ok(response),
            other => Err(SessionClientError::UnexpectedServerMessage { message: other }),
        }
    }

    pub async fn send_prompt(&self, payload: PromptPayload) -> ClientResult<()> {
        self.send_message(&ClientMessage::Prompt(payload)).await
    }

    pub async fn send_mcp_request(&self, payload: McpRequest) -> ClientResult<McpResponse> {
        self.send_message(&ClientMessage::Mcp(payload)).await?;
        match self.read_server_message().await? {
            ServerMessage::Mcp(response) => Ok(response),
            other => Err(SessionClientError::UnexpectedServerMessage { message: other }),
        }
    }

    pub async fn send_prompt_wait_response(&self, payload: PromptPayload) -> ClientResult<String> {
        self.send_message(&ClientMessage::Prompt(payload)).await?;
        match self.read_server_message().await? {
            ServerMessage::PromptResponse(response) => Ok(response),
            other => Err(SessionClientError::UnexpectedServerMessage { message: other }),
        }
    }

    pub async fn begin_record(&self, payload: RecordStartPayload) -> ClientResult<RecordSession> {
        self.send_message(&ClientMessage::RecordStart(payload))
            .await?;
        Ok(RecordSession {
            client: self.clone(),
            allowed: false,
        })
    }

    pub async fn send_message_best_effort(&self, message: ClientMessage) -> bool {
        match self.send_message(&message).await {
            Ok(_) => true,
            Err(err) => {
                warn!(?err, "Failed to send session message");
                false
            }
        }
    }

    async fn send_message(&self, message: &ClientMessage) -> ClientResult<()> {
        let payload = serde_json::to_vec(message).context(session_client_error::SerializeSnafu)?;
        let len = payload.len() as u32;

        let mut writer = self.writer.lock().await;
        writer
            .write_u32(len)
            .await
            .context(session_client_error::SendSnafu {
                socket_path: self.socket_path.clone(),
            })?;
        writer
            .write_all(&payload)
            .await
            .context(session_client_error::SendSnafu {
                socket_path: self.socket_path.clone(),
            })?;
        writer
            .flush()
            .await
            .context(session_client_error::SendSnafu {
                socket_path: self.socket_path.clone(),
            })?;
        Ok(())
    }

    async fn read_server_message(&self) -> ClientResult<ServerMessage> {
        let mut reader = self.reader.lock().await;
        let len = reader
            .read_u32()
            .await
            .context(session_client_error::ReceiveSnafu {
                socket_path: self.socket_path.clone(),
            })?;

        let mut buf = vec![0u8; len as usize];
        reader
            .read_exact(&mut buf)
            .await
            .context(session_client_error::ReceiveSnafu {
                socket_path: self.socket_path.clone(),
            })?;
        serde_json::from_slice(&buf).context(session_client_error::DeserializeSnafu)
    }
}

#[derive(Debug, Clone)]
pub struct RecordSession {
    client: SessionSocketClient,
    allowed: bool,
}

impl RecordSession {
    pub async fn wait_for_allow(&mut self) -> ClientResult<()> {
        if self.allowed {
            return Ok(());
        }
        loop {
            let message = self.client.read_server_message().await?;
            match message {
                ServerMessage::RecordControl(RecordControl {
                    action: ControlAction::Allow,
                }) => {
                    self.allowed = true;
                    debug!("record execution allowed by server");
                    return Ok(());
                }
                ServerMessage::RecordControl(RecordControl {
                    action: ControlAction::Interrupt,
                }) => {
                    debug!("record execution interrupted by server");
                    return Err(SessionClientError::Interrupted);
                }
                other => {
                    debug!(?other, "unexpected server message while waiting for allow");
                }
            }
        }
    }

    pub async fn send_chunk(&self, chunk: RecordChunkPayload) -> ClientResult<()> {
        self.client
            .send_message(&ClientMessage::RecordChunk(chunk))
            .await
    }

    pub async fn finish(&self, payload: RecordEndPayload) -> ClientResult<()> {
        self.client
            .send_message(&ClientMessage::RecordEnd(payload))
            .await
    }

    pub async fn listen_for_interrupt(self) -> ClientResult<()> {
        loop {
            if let ServerMessage::RecordControl(RecordControl {
                action: ControlAction::Interrupt,
            }) = self.client.read_server_message().await?
            {
                return Err(SessionClientError::Interrupted);
            }
        }
    }
}

#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum SessionServerError {
    #[snafu(display("failed to bind session socket at {path:?}: {source}"))]
    Bind {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to accept session connection on {path:?}: {source}"))]
    Accept {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to read client message on {path:?}: {source}"))]
    Receive {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to deserialize client message"))]
    Deserialize { source: serde_json::Error },

    #[snafu(display("failed to serialize server message"))]
    Serialize { source: serde_json::Error },

    #[snafu(display("failed to send server message on {path:?}: {source}"))]
    Send {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub type ServerResult<T, E = SessionServerError> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct SessionSocketServer {
    path: PathBuf,
    listener: UnixListener,
}

impl SessionSocketServer {
    pub async fn bind(path: impl AsRef<Path>) -> ServerResult<Self> {
        let path = path.as_ref().to_path_buf();
        let listener = UnixListener::bind(&path)
            .context(session_server_error::BindSnafu { path: path.clone() })?;
        Ok(Self { path, listener })
    }

    pub async fn accept(&self) -> ServerResult<ServerConnection> {
        let (stream, _) =
            self.listener
                .accept()
                .await
                .context(session_server_error::AcceptSnafu {
                    path: self.path.clone(),
                })?;
        Ok(ServerConnection::new(self.path.clone(), stream))
    }
}

#[derive(Debug)]
pub struct ServerConnection {
    path: PathBuf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl ServerConnection {
    fn new(path: PathBuf, stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            path,
            reader: BufReader::new(read_half),
            writer: write_half,
        }
    }

    pub async fn read_client_message(&mut self) -> ServerResult<ClientMessage> {
        let len = self
            .reader
            .read_u32()
            .await
            .context(session_server_error::ReceiveSnafu {
                path: self.path.clone(),
            })?;
        let mut buf = vec![0u8; len as usize];
        self.reader
            .read_exact(&mut buf)
            .await
            .context(session_server_error::ReceiveSnafu {
                path: self.path.clone(),
            })?;
        serde_json::from_slice(&buf).context(session_server_error::DeserializeSnafu)
    }

    pub async fn send_server_message(&mut self, message: &ServerMessage) -> ServerResult<()> {
        let payload = serde_json::to_vec(message).context(session_server_error::SerializeSnafu)?;
        let len = payload.len() as u32;
        self.writer
            .write_u32(len)
            .await
            .context(session_server_error::SendSnafu {
                path: self.path.clone(),
            })?;
        self.writer
            .write_all(&payload)
            .await
            .context(session_server_error::SendSnafu {
                path: self.path.clone(),
            })?;
        self.writer
            .flush()
            .await
            .context(session_server_error::SendSnafu {
                path: self.path.clone(),
            })?;
        Ok(())
    }

    pub async fn allow(&mut self) -> ServerResult<()> {
        self.send_server_message(&ServerMessage::RecordControl(RecordControl {
            action: ControlAction::Allow,
        }))
        .await
    }

    pub async fn interrupt(&mut self) -> ServerResult<()> {
        self.send_server_message(&ServerMessage::RecordControl(RecordControl {
            action: ControlAction::Interrupt,
        }))
        .await
    }
}

#[cfg(test)]
mod tests {
    use snafu::ResultExt;
    use tokio::task;

    use crate::error::Result;
    use crate::test_utils::preferred_temp_dir;

    use super::*;

    fn unique_socket_path() -> Result<(tempfile::TempDir, String)> {
        let dir = tempfile::Builder::new()
            .prefix("coco-")
            .tempdir_in(preferred_temp_dir())
            .whatever_context("failed to create tempdir")?;
        let path = dir
            .path()
            .join(format!("{}.sock", uuid::Uuid::new_v4().as_simple()));
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }
        let path = path.to_string_lossy().to_string();
        ensure_whatever!(
            path.len() < 100,
            "socket path length must be less than SUN_LEN"
        );
        Ok((dir, path))
    }

    #[tokio::test]
    #[snafu::report]
    async fn send_metadata_over_socket() -> Result<()> {
        let (_dir, socket_path) = unique_socket_path()?;
        let server = SessionSocketServer::bind(&socket_path)
            .await
            .whatever_context("failed to bind socket")?;

        let payload = MetadataPayload {
            name: "commit".to_string(),
            description: Some("Git Commit with Proper Message".to_string()),
            model: None,
            tools: None,
        };
        let client = SessionSocketClient::connect(&socket_path)
            .await
            .whatever_context("failed to connect socket path")?;

        let send_payload = payload.clone();
        let send_task = tokio::spawn(async move {
            client
                .send_metadata_wait_response(send_payload)
                .await
                .expect("send metadata with response")
        });

        let mut conn = server.accept().await.whatever_context("failed to accept")?;
        let event = conn
            .read_client_message()
            .await
            .whatever_context("failed to read client message")?;
        assert_eq!(event, ClientMessage::Metadata(payload));

        conn.send_server_message(&ServerMessage::Metadata(MetadataResponse {
            discovery: false,
        }))
        .await
        .whatever_context("failed to send metadata response")?;

        let response = send_task.await.whatever_context("failed to join")?;
        assert!(!response.discovery);

        Ok(())
    }

    #[tokio::test]
    #[snafu::report]
    async fn send_prompt_wait_response_over_socket() -> Result<()> {
        let (_dir, socket_path) = unique_socket_path()?;
        let server = SessionSocketServer::bind(&socket_path)
            .await
            .whatever_context("failed to bind socket")?;

        let payload = PromptPayload {
            prompt: "Hello".to_string(),
            reply: true,
            schemas: vec![PromptSchema {
                name: "message".to_string(),
                description: "reply message".to_string(),
            }],
        };
        let client = SessionSocketClient::connect(&socket_path)
            .await
            .whatever_context("failed to connect socket path")?;

        let send_payload = payload.clone();
        let send_task = tokio::spawn(async move {
            client
                .send_prompt_wait_response(send_payload)
                .await
                .expect("send prompt with response")
        });

        let mut conn = server.accept().await.whatever_context("failed to accept")?;
        let event = conn
            .read_client_message()
            .await
            .whatever_context("failed to read client message")?;
        assert_eq!(event, ClientMessage::Prompt(payload));

        let response = r#"{"message":"ok"}"#.to_string();
        conn.send_server_message(&ServerMessage::PromptResponse(response.clone()))
            .await
            .whatever_context("failed to send prompt response")?;

        let received = send_task.await.whatever_context("failed to join")?;
        assert_eq!(received, response);

        Ok(())
    }

    #[tokio::test]
    #[snafu::report]
    async fn record_waits_for_allow_then_sends_end() -> Result<()> {
        let (_dir, socket_path) = unique_socket_path()?;
        let server = SessionSocketServer::bind(&socket_path)
            .await
            .whatever_context("failed to bind socket")?;

        let client = SessionSocketClient::connect(&socket_path)
            .await
            .whatever_context("failed to connect socket path")?;

        let start = RecordStartPayload {
            command: vec!["echo".into(), "hi".into()],
            started_at: 1,
        };

        let mut session = client
            .begin_record(start.clone())
            .await
            .whatever_context("failed to begin record")?;

        let server = tokio::spawn(async move {
            let mut conn = server.accept().await.whatever_context("failed to accept")?;

            let start_msg = conn
                .read_client_message()
                .await
                .whatever_context("failed to read start message")?;
            assert_eq!(start_msg, ClientMessage::RecordStart(start));

            conn.allow()
                .await
                .whatever_context("failed to send allow")?;

            let end_msg = conn
                .read_client_message()
                .await
                .whatever_context("failed to read end message")?;
            assert_eq!(
                end_msg,
                ClientMessage::RecordEnd(RecordEndPayload {
                    exit_code: Some(0),
                    stdout: Some("hi\n".into()),
                    stderr: None,
                    ended_at: 2
                })
            );
            Ok::<_, crate::error::Error>(())
        });

        session
            .wait_for_allow()
            .await
            .whatever_context("failed to wait for allow")?;
        session
            .finish(RecordEndPayload {
                exit_code: Some(0),
                stdout: Some("hi\n".into()),
                stderr: None,
                ended_at: 2,
            })
            .await
            .whatever_context("failed to send record end")?;

        server
            .await
            .whatever_context("failed to join server task")??;
        Ok(())
    }

    #[tokio::test]
    #[snafu::report]
    async fn record_interrupt_returns_error() -> Result<()> {
        let (_dir, socket_path) = unique_socket_path()?;
        let server = SessionSocketServer::bind(&socket_path)
            .await
            .whatever_context("failed to bind socket")?;

        let client = SessionSocketClient::connect(&socket_path)
            .await
            .whatever_context("failed to connect socket path")?;
        let mut session = client
            .begin_record(RecordStartPayload {
                command: vec!["echo".into()],
                started_at: 1,
            })
            .await
            .whatever_context("failed to begin record")?;

        let server = tokio::spawn(async move {
            let mut conn = server.accept().await.whatever_context("failed to accept")?;
            let start_msg = conn
                .read_client_message()
                .await
                .whatever_context("failed to read start message")?;
            assert!(matches!(start_msg, ClientMessage::RecordStart(_)));

            conn.interrupt()
                .await
                .whatever_context("failed to send interrupt")?;
            Ok::<_, crate::error::Error>(())
        });

        let result = session.wait_for_allow().await;
        assert!(matches!(result, Err(SessionClientError::Interrupted)));

        server
            .await
            .whatever_context("failed to join server task")??;
        Ok(())
    }

    #[tokio::test]
    #[snafu::report]
    async fn server_accepts_and_replies_allow() -> Result<()> {
        let (_dir, socket_path) = unique_socket_path()?;
        let server = SessionSocketServer::bind(&socket_path)
            .await
            .whatever_context("failed to bind socket")?;

        let server_task = task::spawn(async move {
            let mut conn = server.accept().await.whatever_context("failed to accept")?;
            let msg = conn
                .read_client_message()
                .await
                .whatever_context("failed to read client message")?;
            assert!(matches!(msg, ClientMessage::RecordStart(_)));
            conn.allow()
                .await
                .whatever_context("failed to send allow")?;
            Ok::<_, crate::error::Error>(())
        });

        let client = SessionSocketClient::connect(&socket_path)
            .await
            .whatever_context("failed to connect socket path")?;
        let mut session = client
            .begin_record(RecordStartPayload {
                command: vec!["echo".into()],
                started_at: 1,
            })
            .await
            .whatever_context("failed to begin record")?;
        session
            .wait_for_allow()
            .await
            .whatever_context("failed to wait for allow")?;

        server_task
            .await
            .whatever_context("failed to join server task")??;
        Ok(())
    }
}
