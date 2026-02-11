use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use rmcp::{
    RoleClient,
    model::{CallToolRequestParam, Tool},
    service::{ClientInitializeError, Peer, RunningService, ServiceError, ServiceExt},
    transport::{StreamableHttpClientTransport, TokioChildProcess},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::prelude::*;
use tokio::{process::Command, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    Config, SessionSocketServer,
    combo::{ClientMessage, ServerMessage},
    config::McpConfig,
    logging::{logs_dir, sanitize_log_stem},
};

pub const MCP_SOCKET_ENV: &str = "COCO_MCP_SOCK";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum McpAction {
    ListServers,
    ListTools,
    CallTool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpRequest {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub action: McpAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResponse {
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<McpErrorInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpErrorInfo {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpCallToolPayload {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

impl McpResponse {
    fn ok(request_id: String, result: Value) -> Self {
        Self {
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn err(request_id: String, message: impl Into<String>) -> Self {
        Self {
            request_id,
            ok: false,
            result: None,
            error: Some(McpErrorInfo {
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Snafu)]
pub enum McpManagerError {
    #[snafu(display("unknown mcp server {name}"))]
    UnknownServer { name: String },

    #[snafu(display("failed to resolve env {key} for server {name}: {source}"))]
    ResolveEnv {
        name: String,
        key: String,
        source: crate::Error,
    },

    #[snafu(display("failed to spawn mcp server {name}: {source}"))]
    Spawn {
        name: String,
        source: std::io::Error,
    },

    #[snafu(display("failed to prepare mcp stderr log {} for {name}: {source}", path.display()))]
    LogSetup {
        name: String,
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to initialize mcp client for {name}: {source}"))]
    Initialize {
        name: String,
        source: ClientInitializeError,
    },

    #[snafu(display("mcp request to {name} timed out after {timeout_ms}ms"))]
    Timeout { name: String, timeout_ms: u64 },

    #[snafu(display("mcp request to {name} failed: {source}"))]
    Request { name: String, source: ServiceError },

    #[snafu(display("invalid tool arguments: {reason}"))]
    InvalidArguments { reason: String },
}

type ManagerResult<T, E = McpManagerError> = std::result::Result<T, E>;

#[allow(clippy::result_large_err)]
fn serialize_to_json_value<T: Serialize>(value: T, label: &str) -> ManagerResult<Value> {
    serde_json::to_value(value).map_err(|err| McpManagerError::InvalidArguments {
        reason: format!("failed to serialize {label}: {err}"),
    })
}

#[allow(clippy::result_large_err)]
fn parse_call_tool_payload(payload: Option<Value>) -> ManagerResult<McpCallToolPayload> {
    match payload {
        Some(value) => serde_json::from_value::<McpCallToolPayload>(value).map_err(|err| {
            McpManagerError::InvalidArguments {
                reason: format!("invalid call_tool payload: {err}"),
            }
        }),
        None => Err(McpManagerError::InvalidArguments {
            reason: "missing call_tool payload".to_string(),
        }),
    }
}

fn require_server_for_action<'a>(
    request_id: &str,
    action_name: &str,
    server: Option<&'a str>,
) -> Result<&'a str, McpResponse> {
    server.ok_or_else(|| {
        McpResponse::err(
            request_id.to_string(),
            format!("server is required for {action_name}"),
        )
    })
}

struct McpClientEntry {
    service: RunningService<RoleClient, ()>,
    peer: Peer<RoleClient>,
    last_used: Instant,
}

pub struct McpManager {
    config: McpConfig,
    clients: Mutex<HashMap<String, McpClientEntry>>,
}

impl McpManager {
    pub fn new(config: McpConfig) -> Self {
        Self {
            config,
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub fn list_servers(&self) -> Vec<McpServerInfo> {
        self.config
            .servers
            .iter()
            .map(|server| McpServerInfo {
                name: server.name.clone(),
                description: server.description.clone(),
            })
            .collect()
    }

    pub async fn list_tools(
        &self,
        server: &str,
        timeout_ms: Option<u64>,
    ) -> ManagerResult<Vec<McpToolInfo>> {
        let peer = self.get_peer(server).await?;
        let tools = self
            .with_timeout(server, timeout_ms, peer.list_all_tools())
            .await?;
        Ok(tools.into_iter().map(tool_to_info).collect())
    }

    pub async fn call_tool(
        &self,
        server: &str,
        payload: McpCallToolPayload,
        timeout_ms: Option<u64>,
    ) -> ManagerResult<Value> {
        let peer = self.get_peer(server).await?;
        let arguments = match payload.arguments {
            None | Some(Value::Null) => None,
            Some(Value::Object(map)) => Some(map),
            Some(_) => {
                return Err(McpManagerError::InvalidArguments {
                    reason: "arguments must be a JSON object".to_string(),
                });
            }
        };
        let result = self
            .with_timeout(
                server,
                timeout_ms,
                peer.call_tool(CallToolRequestParam {
                    name: payload.tool.into(),
                    arguments,
                }),
            )
            .await?;
        let value = serialize_to_json_value(result, "tool result")?;
        Ok(value)
    }

    async fn get_peer(&self, server: &str) -> ManagerResult<Peer<RoleClient>> {
        self.cleanup_idle().await;
        let mut clients = self.clients.lock().await;
        if let Some(entry) = clients.get_mut(server) {
            entry.last_used = Instant::now();
            return Ok(entry.peer.clone());
        }
        let config = self
            .config
            .servers
            .iter()
            .find(|cfg| cfg.name == server)
            .cloned()
            .ok_or_else(|| McpManagerError::UnknownServer {
                name: server.to_string(),
            })?;
        drop(clients);

        let service = self.spawn_service(&config).await?;
        let peer = service.peer().clone();
        let mut clients = self.clients.lock().await;
        if let Some(existing) = clients.get_mut(server) {
            existing.last_used = Instant::now();
            let peer = existing.peer.clone();
            drop(clients);
            let _ = service.cancel().await;
            return Ok(peer);
        }
        clients.insert(
            server.to_string(),
            McpClientEntry {
                peer: peer.clone(),
                service,
                last_used: Instant::now(),
            },
        );
        Ok(peer)
    }

    async fn spawn_service(
        &self,
        config: &crate::McpServerConfig,
    ) -> ManagerResult<RunningService<RoleClient, ()>> {
        match &config.connection {
            crate::McpServerConnection::Command(command) => {
                self.spawn_command_service(&config.name, command).await
            }
            crate::McpServerConnection::Http(http) => {
                self.spawn_http_service(&config.name, http).await
            }
        }
    }

    async fn spawn_command_service(
        &self,
        name: &str,
        config: &crate::McpServerCommandConfig,
    ) -> ManagerResult<RunningService<RoleClient, ()>> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        if let Some(cwd) = &config.cwd {
            cmd.current_dir(cwd);
        }
        if let Some(envs) = &config.env {
            for (key, value) in envs {
                let mut value = value.clone();
                let value = value.get().context(ResolveEnvSnafu {
                    name: name.to_string(),
                    key: key.clone(),
                })?;
                cmd.env(key, value);
            }
        }
        let logs_dir = logs_dir();
        fs::create_dir_all(logs_dir).context(LogSetupSnafu {
            name: name.to_string(),
            path: logs_dir.to_path_buf(),
        })?;
        let file_stem = sanitize_log_stem(name);
        let file_name = if file_stem.is_empty() {
            "mcp.log".to_string()
        } else {
            format!("mcp-{file_stem}.log")
        };
        let log_path = logs_dir.join(file_name);
        let log_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .context(LogSetupSnafu {
                name: name.to_string(),
                path: log_path.clone(),
            })?;
        let (transport, _stderr) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::from(log_file))
            .spawn()
            .context(SpawnSnafu {
                name: name.to_string(),
            })?;
        ().serve(transport).await.context(InitializeSnafu {
            name: name.to_string(),
        })
    }

    async fn spawn_http_service(
        &self,
        name: &str,
        config: &crate::McpServerHttpConfig,
    ) -> ManagerResult<RunningService<RoleClient, ()>> {
        let transport = StreamableHttpClientTransport::from_uri(config.url.clone());
        ().serve(transport).await.context(InitializeSnafu {
            name: name.to_string(),
        })
    }

    async fn cleanup_idle(&self) {
        let ttl_ms = self.config.idle_ttl_ms;
        if ttl_ms == 0 {
            return;
        }
        let ttl = Duration::from_millis(ttl_ms);
        let now = Instant::now();
        let mut to_close = Vec::new();
        {
            let mut clients = self.clients.lock().await;
            let expired: Vec<String> = clients
                .iter()
                .filter(|(_, entry)| now.duration_since(entry.last_used) > ttl)
                .map(|(name, _)| name.clone())
                .collect();
            for name in expired {
                if let Some(entry) = clients.remove(&name) {
                    to_close.push(entry.service);
                }
            }
        }
        for service in to_close {
            let _ = service.cancel().await;
        }
    }

    async fn with_timeout<T>(
        &self,
        server: &str,
        timeout_ms: Option<u64>,
        fut: impl std::future::Future<Output = Result<T, ServiceError>>,
    ) -> ManagerResult<T> {
        let timeout_ms = timeout_ms.unwrap_or(self.config.request_timeout_ms);
        if timeout_ms == 0 {
            return fut.await.context(RequestSnafu {
                name: server.to_string(),
            });
        }
        match tokio::time::timeout(Duration::from_millis(timeout_ms), fut).await {
            Ok(result) => result.context(RequestSnafu {
                name: server.to_string(),
            }),
            Err(_) => Err(McpManagerError::Timeout {
                name: server.to_string(),
                timeout_ms,
            }),
        }
    }
}

fn tool_to_info(tool: Tool) -> McpToolInfo {
    let input_schema = Value::Object((*tool.input_schema).clone());
    McpToolInfo {
        name: tool.name.to_string(),
        description: tool.description.map(|desc| desc.to_string()),
        input_schema,
    }
}

pub struct McpSocketServer {
    socket_path: PathBuf,
    shutdown: CancellationToken,
    join_handle: tokio::task::JoinHandle<()>,
}

impl McpSocketServer {
    pub async fn start(config: McpConfig, config_dir: &Path) -> crate::Result<Self> {
        let socket_path = config.resolved_socket_path(config_dir);
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).whatever_context("failed to create mcp socket dir")?;
        }
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)
                .whatever_context("failed to remove old mcp socket")?;
        }
        let server = SessionSocketServer::bind(&socket_path)
            .await
            .whatever_context("failed to bind mcp socket")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&socket_path, perms);
        }

        let manager = std::sync::Arc::new(McpManager::new(config));
        let shutdown = CancellationToken::new();
        let shutdown_task = shutdown.clone();
        let join_handle = tokio::spawn(async move {
            loop {
                let accept = tokio::select! {
                    _ = shutdown_task.cancelled() => break,
                    accept = server.accept() => accept,
                };
                let mut conn = match accept {
                    Ok(conn) => conn,
                    Err(err) => {
                        warn!(?err, "failed to accept mcp connection");
                        continue;
                    }
                };
                let manager = manager.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(&mut conn, manager).await {
                        debug!(?err, "mcp connection closed");
                    }
                });
            }
        });

        Ok(Self {
            socket_path,
            shutdown,
            join_handle,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.join_handle.await;
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

pub async fn start_mcp_server(config: &Config) -> crate::Result<Option<McpSocketServer>> {
    let Some(mcp) = config.mcp.clone() else {
        return Ok(None);
    };
    let server = McpSocketServer::start(mcp, &config.config_dir).await?;
    Ok(Some(server))
}

async fn handle_connection(
    conn: &mut crate::ServerConnection,
    manager: std::sync::Arc<McpManager>,
) -> crate::Result<()> {
    loop {
        let message = match conn.read_client_message().await {
            Ok(message) => message,
            Err(err) => {
                return Err(err).whatever_context("failed to read mcp request");
            }
        };
        match message {
            ClientMessage::Mcp(request) => {
                let response = handle_request(manager.clone(), request).await;
                conn.send_server_message(&ServerMessage::Mcp(response))
                    .await
                    .whatever_context("failed to send mcp response")?;
            }
            other => {
                let request_id = "unknown".to_string();
                let response =
                    McpResponse::err(request_id, format!("unexpected session message: {other:?}"));
                conn.send_server_message(&ServerMessage::Mcp(response))
                    .await
                    .whatever_context("failed to send mcp response")?;
            }
        }
    }
}

async fn handle_request(manager: std::sync::Arc<McpManager>, request: McpRequest) -> McpResponse {
    let McpRequest {
        request_id,
        server,
        action,
        payload,
        timeout_ms,
    } = request;
    let result = match action {
        McpAction::ListServers => {
            let list = manager.list_servers();
            serialize_to_json_value(list, "servers")
        }
        McpAction::ListTools => {
            let server =
                match require_server_for_action(&request_id, "list_tools", server.as_deref()) {
                    Ok(server) => server,
                    Err(response) => return response,
                };
            manager
                .list_tools(server, timeout_ms)
                .await
                .and_then(|tools| serialize_to_json_value(tools, "tools"))
        }
        McpAction::CallTool => {
            let server =
                match require_server_for_action(&request_id, "call_tool", server.as_deref()) {
                    Ok(server) => server,
                    Err(response) => return response,
                };
            let payload = parse_call_tool_payload(payload);
            let payload = match payload {
                Ok(payload) => payload,
                Err(err) => return McpResponse::err(request_id.clone(), err.to_string()),
            };
            manager.call_tool(server, payload, timeout_ms).await
        }
    };

    match result {
        Ok(value) => McpResponse::ok(request_id, value),
        Err(err) => McpResponse::err(request_id, err.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use hyper::server::conn::http1;
    use hyper_util::{rt::TokioIo, service::TowerToHyperService};
    use rmcp::{
        ServerHandler,
        handler::server::{router::tool::ToolRouter, wrapper::Parameters},
        model::{ServerCapabilities, ServerInfo},
        schemars::JsonSchema,
        tool, tool_handler, tool_router,
        transport::{
            StreamableHttpServerConfig,
            streamable_http_server::{
                session::local::LocalSessionManager, tower::StreamableHttpService,
            },
        },
    };
    use serde::Deserialize;
    use snafu::prelude::*;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;
    use tracing::{debug, warn};

    use crate::test_utils::preferred_temp_dir;
    use crate::{
        McpServerCommandConfig, McpServerConfig, McpServerConnection, McpServerHttpConfig,
        error::Result,
    };

    use super::*;

    #[derive(Debug, Deserialize, JsonSchema)]
    struct EchoRequest {
        message: String,
    }

    #[derive(Debug, Clone)]
    struct TestMcpServer {
        tool_router: ToolRouter<Self>,
    }

    impl TestMcpServer {
        fn new() -> Self {
            Self {
                tool_router: Self::tool_router(),
            }
        }
    }

    #[tool_router]
    impl TestMcpServer {
        #[tool(description = "Echo a message")]
        fn echo(&self, Parameters(EchoRequest { message }): Parameters<EchoRequest>) -> String {
            format!("echo: {message}")
        }

        #[tool(description = "Ping the server")]
        fn ping(&self) -> String {
            "pong".to_string()
        }
    }

    #[tool_handler]
    impl ServerHandler for TestMcpServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo {
                instructions: Some("Test MCP server".to_string()),
                capabilities: ServerCapabilities::builder().enable_tools().build(),
                ..Default::default()
            }
        }
    }

    pub(crate) struct TestHttpServer {
        base_url: String,
        shutdown: CancellationToken,
        join_handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl TestHttpServer {
        pub(crate) async fn start() -> Result<Self> {
            let session_manager = Arc::new(LocalSessionManager::default());
            let service: StreamableHttpService<TestMcpServer, LocalSessionManager> =
                StreamableHttpService::new(
                    || Ok(TestMcpServer::new()),
                    session_manager,
                    StreamableHttpServerConfig {
                        stateful_mode: true,
                        sse_keep_alive: None,
                        cancellation_token: CancellationToken::new(),
                    },
                );
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .whatever_context("failed to bind http listener")?;
            let addr = listener
                .local_addr()
                .whatever_context("failed to get http listener address")?;
            let shutdown = CancellationToken::new();
            let join_handle = tokio::spawn({
                let shutdown = shutdown.clone();
                let service = service.clone();
                async move {
                    loop {
                        let accept = tokio::select! {
                            _ = shutdown.cancelled() => break,
                            accept = listener.accept() => accept,
                        };
                        let (stream, _) = match accept {
                            Ok(accept) => accept,
                            Err(_) => break,
                        };
                        let service = TowerToHyperService::new(service.clone());
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let _ = http1::Builder::new().serve_connection(io, service).await;
                        });
                    }
                }
            });
            Ok(Self {
                base_url: format!("http://{addr}/mcp"),
                shutdown,
                join_handle: Some(join_handle),
            })
        }

        pub(crate) async fn shutdown(mut self) {
            self.shutdown.cancel();
            if let Some(handle) = self.join_handle.take() {
                let _ = handle.await;
            }
        }

        pub(crate) fn base_url(&self) -> &str {
            &self.base_url
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            self.shutdown.cancel();
            if let Some(handle) = self.join_handle.take() {
                handle.abort();
            }
        }
    }

    pub(crate) fn test_config(server_name: &str, description: Option<&str>) -> McpConfig {
        McpConfig {
            socket_path: PathBuf::from("mcp.sock"),
            request_timeout_ms: 5_000,
            idle_ttl_ms: 0,
            servers: vec![McpServerConfig {
                name: server_name.to_string(),
                description: description.map(|value| value.to_string()),
                connection: McpServerConnection::Command(McpServerCommandConfig {
                    command: "unused".to_string(),
                    args: Vec::new(),
                    cwd: None,
                    env: None,
                }),
            }],
        }
    }

    pub(crate) async fn install_peer(
        manager: &McpManager,
        server_name: &str,
        service: RunningService<RoleClient, ()>,
    ) {
        let peer = service.peer().clone();
        let mut clients = manager.clients.lock().await;
        clients.insert(
            server_name.to_string(),
            McpClientEntry {
                peer,
                service,
                last_used: Instant::now(),
            },
        );
    }

    pub(crate) async fn remove_peer(manager: &McpManager, server_name: &str) {
        let entry = manager.clients.lock().await.remove(server_name);
        if let Some(entry) = entry {
            let _ = entry.service.cancel().await;
        }
    }

    pub(crate) struct TestMcpSocketServer {
        socket_path: PathBuf,
        shutdown: CancellationToken,
        join_handle: Option<tokio::task::JoinHandle<()>>,
        _temp_dir: tempfile::TempDir,
    }

    impl TestMcpSocketServer {
        pub(crate) async fn start(manager: Arc<McpManager>) -> Result<Self> {
            let (_temp_dir, socket_path) = unique_socket_path()?;
            if socket_path.exists() {
                let _ = std::fs::remove_file(&socket_path);
            }
            let server = SessionSocketServer::bind(&socket_path)
                .await
                .whatever_context("failed to bind mcp socket")?;
            let shutdown = CancellationToken::new();
            let join_handle = tokio::spawn({
                let shutdown = shutdown.clone();
                async move {
                    loop {
                        let accept = tokio::select! {
                            _ = shutdown.cancelled() => break,
                            accept = server.accept() => accept,
                        };
                        let mut conn = match accept {
                            Ok(conn) => conn,
                            Err(err) => {
                                warn!(?err, "failed to accept mcp connection");
                                continue;
                            }
                        };
                        let manager = manager.clone();
                        tokio::spawn(async move {
                            if let Err(err) = handle_connection(&mut conn, manager).await {
                                debug!(?err, "mcp connection closed");
                            }
                        });
                    }
                }
            });

            Ok(Self {
                socket_path,
                shutdown,
                join_handle: Some(join_handle),
                _temp_dir,
            })
        }

        pub(crate) fn socket_path(&self) -> &Path {
            &self.socket_path
        }

        pub(crate) async fn shutdown(mut self) {
            self.shutdown.cancel();
            if let Some(handle) = self.join_handle.take() {
                let _ = handle.await;
            }
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    impl Drop for TestMcpSocketServer {
        fn drop(&mut self) {
            self.shutdown.cancel();
            if let Some(handle) = self.join_handle.take() {
                handle.abort();
            }
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    pub(crate) struct TestDropGuard {
        actions: Vec<TestDropAction>,
    }

    enum TestDropAction {
        Peer {
            manager: Arc<McpManager>,
            server_name: String,
        },
        HttpServer(TestHttpServer),
        SocketServer(TestMcpSocketServer),
    }

    impl TestDropGuard {
        pub(crate) fn new() -> Self {
            Self {
                actions: Vec::new(),
            }
        }

        pub(crate) fn add_peer(
            &mut self,
            manager: Arc<McpManager>,
            server_name: impl Into<String>,
        ) {
            self.actions.push(TestDropAction::Peer {
                manager,
                server_name: server_name.into(),
            });
        }

        pub(crate) fn add_http_server(&mut self, server: TestHttpServer) {
            self.actions.push(TestDropAction::HttpServer(server));
        }

        pub(crate) fn add_socket_server(&mut self, server: TestMcpSocketServer) {
            self.actions.push(TestDropAction::SocketServer(server));
        }

        pub(crate) async fn shutdown(mut self) {
            // Unwind in reverse registration order to avoid dependency issues.
            while let Some(action) = self.actions.pop() {
                action.shutdown().await;
            }
        }
    }

    impl TestDropAction {
        async fn shutdown(self) {
            match self {
                TestDropAction::Peer {
                    manager,
                    server_name,
                } => {
                    remove_peer(&manager, &server_name).await;
                }
                TestDropAction::HttpServer(server) => server.shutdown().await,
                TestDropAction::SocketServer(server) => server.shutdown().await,
            }
        }
    }

    fn unique_socket_path() -> Result<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::Builder::new()
            .prefix("coco-mcp-")
            .tempdir_in(preferred_temp_dir())
            .whatever_context("failed to create tempdir")?;
        let path = dir
            .path()
            .join(format!("{}.sock", uuid::Uuid::new_v4().as_simple()));
        ensure_whatever!(
            path.to_string_lossy().len() < 100,
            "socket path length must be less than SUN_LEN"
        );
        Ok((dir, path))
    }

    #[tokio::test]
    #[snafu::report]
    async fn list_servers_returns_configured_servers() -> Result<()> {
        let config = McpConfig {
            socket_path: PathBuf::from("mcp.sock"),
            request_timeout_ms: 5_000,
            idle_ttl_ms: 0,
            servers: vec![
                McpServerConfig {
                    name: "alpha".to_string(),
                    description: Some("Alpha server".to_string()),
                    connection: McpServerConnection::Command(McpServerCommandConfig {
                        command: "alpha".to_string(),
                        args: Vec::new(),
                        cwd: None,
                        env: None,
                    }),
                },
                McpServerConfig {
                    name: "beta".to_string(),
                    description: None,
                    connection: McpServerConnection::Command(McpServerCommandConfig {
                        command: "beta".to_string(),
                        args: Vec::new(),
                        cwd: None,
                        env: None,
                    }),
                },
            ],
        };
        let manager = McpManager::new(config);
        let servers = manager.list_servers();
        assert_eq!(
            servers,
            vec![
                McpServerInfo {
                    name: "alpha".to_string(),
                    description: Some("Alpha server".to_string()),
                },
                McpServerInfo {
                    name: "beta".to_string(),
                    description: None,
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    #[snafu::report]
    async fn list_tools_and_call_tool_over_http() -> Result<()> {
        let server = TestHttpServer::start().await?;
        let server_url = server.base_url().to_string();
        let config = McpConfig {
            socket_path: PathBuf::from("mcp.sock"),
            request_timeout_ms: 5_000,
            idle_ttl_ms: 0,
            servers: vec![McpServerConfig {
                name: "test".to_string(),
                description: Some("Test server".to_string()),
                connection: McpServerConnection::Http(McpServerHttpConfig { url: server_url }),
            }],
        };
        let manager = Arc::new(McpManager::new(config));
        let mut guard = TestDropGuard::new();
        guard.add_http_server(server);

        let tools = manager
            .list_tools("test", None)
            .await
            .whatever_context("failed to list tools")?;
        assert_eq!(tools.len(), 2);
        assert!(
            tools.iter().any(|tool| tool.name == "echo"),
            "echo tool should be listed"
        );
        assert!(
            tools.iter().any(|tool| tool.name == "ping"),
            "ping tool should be listed"
        );
        let echo_tool = tools
            .iter()
            .find(|tool| tool.name == "echo")
            .expect("echo tool should exist");
        assert!(
            echo_tool.input_schema.get("properties").is_some(),
            "echo tool schema should include properties"
        );

        let result = manager
            .call_tool(
                "test",
                McpCallToolPayload {
                    tool: "echo".to_string(),
                    arguments: Some(serde_json::json!({"message": "hello"})),
                },
                None,
            )
            .await
            .whatever_context("failed to call tool")?;
        let text = result
            .get("content")
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(|value| value.as_str())
            .expect("call_tool result should include text content");
        assert_eq!(text, "echo: hello");

        guard.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    #[snafu::report]
    async fn handle_request_requires_server_and_payload() -> Result<()> {
        let manager = Arc::new(McpManager::new(test_config("test", None)));
        let response = handle_request(
            manager.clone(),
            McpRequest {
                request_id: "req-1".to_string(),
                server: None,
                action: McpAction::ListTools,
                payload: None,
                timeout_ms: None,
            },
        )
        .await;
        assert!(!response.ok);
        assert_eq!(
            response.error.map(|err| err.message),
            Some("server is required for list_tools".to_string())
        );

        let response = handle_request(
            manager,
            McpRequest {
                request_id: "req-2".to_string(),
                server: Some("test".to_string()),
                action: McpAction::CallTool,
                payload: None,
                timeout_ms: None,
            },
        )
        .await;
        assert!(!response.ok);
        assert!(
            response
                .error
                .map(|err| err.message)
                .unwrap_or_default()
                .contains("missing call_tool payload")
        );
        Ok(())
    }
}
