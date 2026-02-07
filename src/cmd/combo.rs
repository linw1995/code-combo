use snafu::prelude::*;

use crate::{
    ComboRunPayload, ComboRunResult, RunComboOutput, SESSION_SOCKET_ENV, SessionSocketClient,
    error::Result,
};

pub async fn handle_combo_run(name: String, args: Vec<String>) -> Result<()> {
    ensure_whatever!(!name.trim().is_empty(), "combo name is required");
    let payload = ComboRunPayload {
        run_id: new_run_id(),
        combo_name: name,
        args,
    };

    let client = SessionSocketClient::from_env()
        .await
        .whatever_context(format!("failed to read {SESSION_SOCKET_ENV}"))?;

    let Some(client) = client else {
        whatever!(
            "{SESSION_SOCKET_ENV} is not set or session socket unavailable; start coco TUI first"
        );
    };

    let result = run_with_client(client, payload).await?;
    emit_result(&result)?;
    if !result.success {
        let error = result
            .error
            .clone()
            .unwrap_or_else(|| "combo run failed".to_string());
        whatever!("{error}");
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

fn new_run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("run_{}_{}", std::process::id(), nanos)
}

#[cfg(test)]
mod tests {
    use crate::test_utils::SessionSocketTestGuard;

    use super::*;

    #[tokio::test]
    async fn combo_run_requires_existing_session_socket() {
        let guard = SessionSocketTestGuard::acquire();
        guard.clear_env();
        guard.clear_global();

        let err = handle_combo_run("demo".to_string(), Vec::new())
            .await
            .expect_err("combo run should fail without session socket");
        let message = err.to_string();
        assert!(
            message.contains("start coco TUI first"),
            "unexpected error: {message}"
        );
    }
}
