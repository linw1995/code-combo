use std::{path::Path, path::PathBuf, process::Stdio, time::Duration};

use snafu::prelude::*;
use tokio::{process::Command, time::Instant};
use tracing::{debug, info, warn};

use crate::{
    ComboRunPayload, ComboRunResult, RunComboOutput, SESSION_SOCKET_ENV, SessionEnv,
    SessionSocketClient, error::Result,
};

pub async fn handle_combo_run(
    name: String,
    args: Vec<String>,
    ignore_workspace_scripts: bool,
) -> Result<()> {
    ensure_whatever!(!name.trim().is_empty(), "combo name is required");
    let payload = ComboRunPayload {
        run_id: new_run_id(),
        combo_name: name,
        args,
    };

    let client = SessionSocketClient::from_env()
        .await
        .whatever_context(format!("failed to read {SESSION_SOCKET_ENV}"))?;

    match client {
        Some(client) => {
            let result = run_with_client(client, payload).await?;
            emit_result(&result)?;
            if !result.success {
                let error = result
                    .error
                    .clone()
                    .unwrap_or_else(|| "combo run failed".to_string());
                whatever!("{error}");
            }
        }
        None => {
            let (result, mut child) = run_with_tui(payload, ignore_workspace_scripts).await?;
            wait_for_tui_exit(&mut child).await?;
            if !result.success {
                let error = result
                    .error
                    .clone()
                    .unwrap_or_else(|| "combo run failed".to_string());
                whatever!("{error}");
            }
        }
    }
    Ok(())
}

async fn run_with_client(
    client: SessionSocketClient,
    payload: ComboRunPayload,
) -> Result<ComboRunResult> {
    let mut session = client
        .begin_combo_run(payload)
        .await
        .whatever_context("failed to start combo run session")?;
    let result = session
        .wait_for_result()
        .await
        .whatever_context("failed to wait for combo run result")?;
    Ok(result)
}

async fn run_with_tui(
    payload: ComboRunPayload,
    ignore_workspace_scripts: bool,
) -> Result<(ComboRunResult, tokio::process::Child)> {
    let session_env = SessionEnv::builder()
        .build()
        .whatever_context("failed to build session env")?;
    let socket_path = session_env.socket_path().to_path_buf();

    let mut child = spawn_tui(&session_env, ignore_workspace_scripts).await?;
    let client = wait_for_session(&socket_path, &mut child).await?;
    info!(?socket_path, "session socket ready for combo run");
    let result = run_with_client(client, payload).await?;
    Ok((result, child))
}

async fn spawn_tui(
    session_env: &SessionEnv,
    ignore_workspace_scripts: bool,
) -> Result<tokio::process::Child> {
    let tui_bin = resolve_tui_command();
    let mut cmd = Command::new(&tui_bin);
    if ignore_workspace_scripts {
        cmd.arg("--ignore-workspace-scripts");
    }
    cmd.env(session_env.socket_env_name(), session_env.socket_path());
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = cmd
        .spawn()
        .whatever_context("failed to start coco TUI process")?;
    Ok(child)
}

async fn wait_for_session(
    socket_path: &Path,
    child: &mut tokio::process::Child,
) -> Result<SessionSocketClient> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match SessionSocketClient::connect(socket_path).await {
            Ok(client) => return Ok(client),
            Err(err) => {
                debug!(?err, "failed to connect to session socket");
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                whatever!(
                    "TUI process exited before session socket was ready: {status} (set COCO_TUI_BIN if needed)"
                );
            }
            Ok(None) => {}
            Err(err) => {
                warn!(?err, "failed to check TUI process status");
            }
        }

        if Instant::now() >= deadline {
            whatever!(
                "timed out waiting for session socket at {:?} (set COCO_TUI_BIN if needed)",
                socket_path
            );
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_tui_exit(child: &mut tokio::process::Child) -> Result<()> {
    let status = child
        .wait()
        .await
        .whatever_context("failed to wait for TUI process")?;
    if !status.success() {
        warn!(?status, "TUI exited with non-zero status");
    }
    Ok(())
}

fn emit_result(result: &ComboRunResult) -> Result<()> {
    let output = RunComboOutput {
        success: result.success,
        summary: result.summary.clone(),
        tool_calls: result.tool_calls,
        error: result.error.clone(),
        summary_thinking: Vec::new(),
    };
    let json =
        serde_json::to_string(&output).whatever_context("failed to serialize combo run output")?;
    println!("{json}");
    Ok(())
}

fn resolve_tui_command() -> PathBuf {
    std::env::var_os("COCO_TUI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("coco"))
}

fn new_run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("run_{}_{}", std::process::id(), nanos)
}
