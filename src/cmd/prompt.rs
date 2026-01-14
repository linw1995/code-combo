use std::collections::HashMap;

use serde_json::Value;
use snafu::prelude::*;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::{PromptPayload, PromptSchema, SessionSocketClient, error::Result};

pub async fn handle_ask(prompt: String, schemas: Vec<String>) -> Result<()> {
    let prompt = resolve_prompt(prompt).await?;
    let explicit_schemas = !schemas.is_empty();
    let mut schemas = parse_prompt_schemas(&schemas)?;
    if schemas.is_empty() {
        schemas.push(default_prompt_schema());
    }

    let Some(client) = SessionSocketClient::from_env()
        .await
        .whatever_context("failed to new from env COCO_SESSION_SOCK")?
    else {
        whatever!("env COCO_SESSION_SOCK is not set");
    };

    let payload = PromptPayload {
        prompt,
        reply: true,
        schemas,
        thinking: None,
    };
    let response = client
        .send_prompt_wait_response(payload)
        .await
        .whatever_context("failed to send prompt and wait response to session socket")?;
    if explicit_schemas {
        println!("{response}");
    } else {
        let message = parse_prompt_message_response(&response)?;
        println!("{message}");
    }

    info!("prompt sent to session socket");
    Ok(())
}

pub async fn handle_tell(prompt: String) -> Result<()> {
    let prompt = resolve_prompt(prompt).await?;
    let Some(client) = SessionSocketClient::from_env()
        .await
        .whatever_context("failed to new from env COCO_SESSION_SOCK")?
    else {
        whatever!("env COCO_SESSION_SOCK is not set");
    };
    let payload = PromptPayload {
        prompt,
        thinking: None,
        ..Default::default()
    };
    client
        .send_prompt(payload)
        .await
        .whatever_context("failed to send prompt to session socket")?;
    info!("prompt sent to session socket");
    Ok(())
}

/// Handle the reply command for combo reply offload.
/// Fields are provided as key=value pairs.
/// If expect_fields is provided, validates that all expected fields are present.
pub async fn handle_reply(expect_fields: Option<String>, fields: Vec<String>) -> Result<()> {
    let parsed_fields = parse_reply_fields(&fields)?;

    // Validate expected fields if specified
    if let Some(expected) = expect_fields {
        let expected_names: Vec<&str> = expected.split(',').map(|s| s.trim()).collect();
        let mut missing = Vec::new();
        for name in &expected_names {
            if !name.is_empty() && !parsed_fields.contains_key(*name) {
                missing.push(*name);
            }
        }
        if !missing.is_empty() {
            eprintln!("Error: missing required fields: {}", missing.join(", "));
            std::process::exit(1);
        }
    }

    // Output the fields as JSON for bash result parsing
    let output = serde_json::to_string(&parsed_fields)
        .whatever_context("failed to serialize reply fields")?;
    println!("{output}");

    info!("reply output generated");
    Ok(())
}

fn parse_reply_fields(fields: &[String]) -> Result<HashMap<String, String>> {
    let mut parsed = HashMap::new();
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            whatever!("invalid field format {field:?}, expected key=value");
        };
        let key = key.trim();
        let value = value.trim();
        ensure_whatever!(!key.is_empty(), "field key cannot be empty");
        if parsed.contains_key(key) {
            whatever!("duplicate field key: {key}");
        }
        parsed.insert(key.to_string(), value.to_string());
    }
    Ok(parsed)
}

async fn resolve_prompt(prompt: String) -> Result<String> {
    if !prompt.trim().is_empty() {
        return Ok(prompt);
    }
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
    Ok(trimmed.to_string())
}

fn default_prompt_schema() -> PromptSchema {
    PromptSchema {
        name: "message".to_string(),
        description: "reply message".to_string(),
    }
}

fn parse_prompt_message_response(response: &str) -> Result<String> {
    let value: Value =
        serde_json::from_str(response).whatever_context("failed to parse prompt reply as JSON")?;
    let Some(object) = value.as_object() else {
        whatever!("prompt reply must be a JSON object");
    };
    let Some(message) = object.get("message").and_then(|value| value.as_str()) else {
        whatever!("prompt reply must include a string field \"message\"");
    };
    Ok(message.to_string())
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

    #[test]
    fn parse_prompt_message_response_ok() {
        let response = r#"{"message":"ok"}"#;
        assert_eq!(parse_prompt_message_response(response).unwrap(), "ok");
    }

    #[test]
    fn parse_prompt_message_response_rejects_missing_message() {
        let response = r#"{"note":"ok"}"#;
        assert!(parse_prompt_message_response(response).is_err());
    }

    #[test]
    fn parse_prompt_message_response_rejects_non_object() {
        let response = r#"["ok"]"#;
        assert!(parse_prompt_message_response(response).is_err());
    }
}
