use snafu::prelude::*;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::{PromptPayload, SessionSocketClient, error::Result};

pub async fn handle_ask(prompt: String) -> Result<()> {
    let prompt = if prompt.trim().is_empty() {
        let mut buf = String::new();
        tokio::io::stdin()
            .read_to_string(&mut buf)
            .await
            .whatever_context("failed to read prompt from stdin")?;
        let trimmed = buf.trim_end_matches(['\n', '\r']);
        let trimmed = trimmed.trim();
        ensure_whatever!(
            !trimmed.is_empty(),
            "prompt is required (provide args or stdin)"
        );
        trimmed.to_string()
    } else {
        prompt
    };

    let Some(client) = SessionSocketClient::from_env()
        .await
        .whatever_context("failed to new from env COCO_SESSION_SOCK")?
    else {
        whatever!("env COCO_SESSION_SOCK is not set");
    };

    client
        .send_prompt(PromptPayload { prompt })
        .await
        .whatever_context("failed to send prompt to session socket")?;

    info!("prompt sent to session socket");
    Ok(())
}
