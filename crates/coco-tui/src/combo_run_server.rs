use std::path::{Path, PathBuf};

use code_combo::{
    ClientMessage, ComboRunMessage, ComboRunPayload, ComboRunResult, SESSION_SOCKET_ENV,
    ServerConnection, ServerMessage, SessionEnv, SessionSocketServer,
};
use snafu::prelude::*;
use tokio::{net::UnixStream, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    actions::{Action, ComboAction},
    combo_run_bridge::ComboRunBridge,
    error::Result,
    global,
};

pub struct ComboRunSessionServer {
    socket_path: PathBuf,
    shutdown: CancellationToken,
    join_handle: JoinHandle<()>,
    _session_env: Option<SessionEnv>,
}

impl ComboRunSessionServer {
    pub async fn start(bridge: &'static ComboRunBridge) -> Result<Option<Self>> {
        let (socket_path, session_env) = resolve_socket_path().await?;
        if let Some(existing) = try_connect_existing(&socket_path).await? {
            warn!(
                socket_path = %existing.display(),
                "session socket already in use; skipping combo run server"
            );
            return Ok(None);
        }
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)
                .whatever_context("failed to remove existing session socket")?;
        }
        let server = SessionSocketServer::bind(&socket_path)
            .await
            .whatever_context("failed to bind session socket")?;
        // Safety: set once during startup before spawning any tasks.
        unsafe {
            std::env::set_var(SESSION_SOCKET_ENV, &socket_path);
        }

        let shutdown = CancellationToken::new();
        let shutdown_task = shutdown.clone();
        let join_handle = tokio::spawn(async move {
            loop {
                let accept = tokio::select! {
                    _ = shutdown_task.cancelled() => break,
                    accept = server.accept() => accept,
                };
                let conn = match accept {
                    Ok(conn) => conn,
                    Err(err) => {
                        warn!(?err, "failed to accept combo run connection");
                        continue;
                    }
                };
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(conn, bridge).await {
                        warn!(?err, "combo run connection closed");
                    }
                });
            }
        });

        Ok(Some(Self {
            socket_path,
            shutdown,
            join_handle,
            _session_env: session_env,
        }))
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

async fn resolve_socket_path() -> Result<(PathBuf, Option<SessionEnv>)> {
    if let Ok(path) = std::env::var(SESSION_SOCKET_ENV) {
        let socket_path = PathBuf::from(path);
        let parent_exists = socket_path
            .parent()
            .map(|parent| parent.exists())
            .unwrap_or(false);
        if parent_exists {
            return Ok((socket_path, None));
        }
        warn!(
            socket_path = %socket_path.display(),
            env = SESSION_SOCKET_ENV,
            "session socket env parent dir missing; creating new session env"
        );
    }
    let env = SessionEnv::builder()
        .build()
        .whatever_context("failed to build session env")?;
    let socket_path = env.socket_path().to_path_buf();
    Ok((socket_path, Some(env)))
}

async fn try_connect_existing(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    match UnixStream::connect(path).await {
        Ok(stream) => {
            drop(stream);
            Ok(Some(path.to_path_buf()))
        }
        Err(err) => {
            debug!(?err, "failed to connect existing session socket");
            Ok(None)
        }
    }
}

async fn handle_connection(
    mut conn: ServerConnection,
    bridge: &'static ComboRunBridge,
) -> Result<()> {
    let message = conn
        .read_client_message()
        .await
        .whatever_context("failed to read client message")?;
    match message {
        ClientMessage::ComboRun(payload) => handle_combo_run(conn, payload, bridge).await,
        other => {
            warn!(?other, "unexpected message for combo run server");
            Ok(())
        }
    }
}

async fn handle_combo_run(
    mut conn: ServerConnection,
    payload: ComboRunPayload,
    bridge: &'static ComboRunBridge,
) -> Result<()> {
    let run_id = payload.run_id.clone();
    let (tx, mut rx) = mpsc::unbounded_channel::<ComboRunMessage>();
    if !bridge.register(run_id.clone(), tx) {
        let result = ComboRunResult {
            run_id,
            success: false,
            summary: "run_id already in use".to_string(),
            tool_calls: 0,
            error: Some("run_id already in use".to_string()),
        };
        conn.send_server_message(&ServerMessage::ComboRunResult(result))
            .await
            .whatever_context("failed to send combo run error")?;
        return Ok(());
    }

    global::action_tx()
        .send(Action::Combo(ComboAction::Execute {
            id: Some(payload.run_id),
            name: payload.combo_name,
            args: payload.args,
        }))
        .ok();

    while let Some(message) = rx.recv().await {
        let send_result = match message {
            ComboRunMessage::Event(event) => {
                conn.send_server_message(&ServerMessage::ComboRunEvent(event))
                    .await
            }
            ComboRunMessage::Result(result) => {
                let send = conn
                    .send_server_message(&ServerMessage::ComboRunResult(result))
                    .await;
                bridge.remove(&run_id);
                return send.whatever_context("failed to send combo run result");
            }
        };
        if let Err(err) = send_result {
            bridge.remove(&run_id);
            return Err(err).whatever_context("failed to send combo run event");
        }
    }

    bridge.remove(&run_id);
    Ok(())
}
