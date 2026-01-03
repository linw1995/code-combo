use snafu::prelude::*;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::{PromptPayload, PromptSchema, SessionSocketClient, error::Result};

pub async fn handle_ask(prompt: String, reply: bool, schemas: Vec<String>) -> Result<()> {
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

    let schemas = parse_prompt_schemas(&schemas)?;
    if reply {
        ensure_whatever!(
            !schemas.is_empty(),
            "--reply requires at least one --schemas"
        );
    } else {
        ensure_whatever!(schemas.is_empty(), "--schemas requires --reply");
    }

    let Some(client) = SessionSocketClient::from_env()
        .await
        .whatever_context("failed to new from env COCO_SESSION_SOCK")?
    else {
        whatever!("env COCO_SESSION_SOCK is not set");
    };

    let payload = PromptPayload {
        prompt,
        reply,
        schemas,
    };
    if reply {
        let response = client
            .send_prompt_wait_response(payload)
            .await
            .whatever_context("failed to send prompt and wait response to session socket")?;
        println!("{response}");
    } else {
        client
            .send_prompt(payload)
            .await
            .whatever_context("failed to send prompt to session socket")?;
    }

    info!("prompt sent to session socket");
    Ok(())
}

fn parse_prompt_schemas(schemas: &[String]) -> Result<Vec<PromptSchema>> {
    let mut parsed = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let Some((name, description)) = schema.split_once(':') else {
            whatever!("invalid schema {schema:?}, expected field:description");
        };
        let name = name.trim();
        let description = description.trim();
        ensure_whatever!(!name.is_empty(), "schema field name cannot be empty");
        ensure_whatever!(
            !parsed.iter().any(|item: &PromptSchema| item.name == name),
            "duplicate schema field: {name}"
        );
        parsed.push(PromptSchema {
            name: name.to_string(),
            description: description.to_string(),
        });
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prompt_schemas_ok() {
        let schemas = vec![
            "message: commit message".to_string(),
            "scope: optional scope".to_string(),
        ];
        let parsed = parse_prompt_schemas(&schemas).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "message");
        assert_eq!(parsed[0].description, "commit message");
        assert_eq!(parsed[1].name, "scope");
    }

    #[test]
    fn parse_prompt_schemas_rejects_missing_colon() {
        let schemas = vec!["message".to_string()];
        assert!(parse_prompt_schemas(&schemas).is_err());
    }

    #[test]
    fn parse_prompt_schemas_rejects_empty_name() {
        let schemas = vec![":desc".to_string()];
        assert!(parse_prompt_schemas(&schemas).is_err());
    }

    #[test]
    fn parse_prompt_schemas_rejects_duplicate() {
        let schemas = vec!["message:one".to_string(), "message:two".to_string()];
        assert!(parse_prompt_schemas(&schemas).is_err());
    }
}
