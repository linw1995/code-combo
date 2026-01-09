use snafu::prelude::*;
use tracing::info;

use crate::{MetadataPayload, MetadataResponse, SessionSocketClient, error::Result};

pub async fn handle_metadata(fields: Vec<String>) -> Result<()> {
    let payload = parse_metadata_fields(&fields)?;
    let name = payload.name.clone();

    let Some(client) = SessionSocketClient::from_env()
        .await
        .whatever_context("failed to new from env COCO_SESSION_SOCK")?
    else {
        whatever!("env COCO_SESSION_SOCK is not set");
    };

    let MetadataResponse { discovery } = client
        .send_metadata_wait_response(payload)
        .await
        .whatever_context("failed to send metadata to session socket")?;

    ensure_whatever!(!discovery, "session is in discovery mode");

    info!(metadata.name = %name, "metadata sent to session socket");

    Ok(())
}

fn parse_metadata_fields(fields: &[String]) -> Result<MetadataPayload> {
    let mut name = None;
    let mut description = None;
    let mut model = None;
    let mut tools: Vec<String> = Vec::new();
    let mut thinking_enabled: Option<bool> = None;
    let mut thinking_budget: Option<usize> = None;

    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            whatever!("invalid metadata entry {field:?}, expected key=value");
        };
        let key = key.trim();
        let value = value.trim();
        ensure_whatever!(!key.is_empty(), "metadata key is empty");

        match key {
            "name" => {
                ensure_whatever!(!value.is_empty(), "metadata name cannot be empty");
                ensure_whatever!(name.is_none(), "duplicate metadata field: name");
                name = Some(value.to_string());
            }
            "description" => {
                ensure_whatever!(
                    description.is_none(),
                    "duplicate metadata field: description"
                );
                description = Some(value.to_string());
            }
            "model" => {
                ensure_whatever!(model.is_none(), "duplicate metadata field: model");
                model = Some(value.to_string());
            }
            "tools" => {
                for tool in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    if !tools.iter().any(|existing| existing == tool) {
                        tools.push(tool.to_string());
                    }
                }
            }
            "thinking" => {
                ensure_whatever!(
                    thinking_enabled.is_none(),
                    "duplicate metadata field: thinking"
                );
                let value = value.to_ascii_lowercase();
                let enabled = matches!(value.as_str(), "1" | "true" | "yes" | "on");
                let disabled = matches!(value.as_str(), "0" | "false" | "no" | "off");
                ensure_whatever!(enabled || disabled, "invalid thinking value: {value}");
                thinking_enabled = Some(enabled);
            }
            "thinking_budget" => {
                ensure_whatever!(
                    thinking_budget.is_none(),
                    "duplicate metadata field: thinking_budget"
                );
                let parsed: usize = value
                    .parse()
                    .whatever_context("thinking_budget must be an integer")?;
                thinking_budget = Some(parsed);
            }
            other => whatever!("unknown metadata field: {other}"),
        }
    }

    let Some(name) = name else {
        whatever!("missing required metadata field: name");
    };

    let tools = if tools.is_empty() { None } else { Some(tools) };

    let thinking = if thinking_enabled.is_some() || thinking_budget.is_some() {
        Some(crate::ThinkingConfig {
            enabled: thinking_enabled.unwrap_or(true),
            budget_tokens: thinking_budget,
        })
    } else {
        None
    };

    Ok(MetadataPayload {
        name,
        description,
        model,
        tools,
        thinking,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_metadata() {
        let payload = parse_metadata_fields(&[String::from("name=commit")]).unwrap();
        assert_eq!(payload.name, "commit");
        assert!(payload.description.is_none());
        assert!(payload.model.is_none());
        assert!(payload.tools.is_none());
    }

    #[test]
    fn parse_full_metadata_with_tools() {
        let payload = parse_metadata_fields(&[
            String::from("name=commit"),
            String::from("description=Git commit helper"),
            String::from("model=claude-3-opus"),
            String::from("tools=git status,git add ,git commit"),
        ])
        .unwrap();
        assert_eq!(payload.name, "commit");
        assert_eq!(payload.description, Some(String::from("Git commit helper")));
        assert_eq!(payload.model, Some(String::from("claude-3-opus")));
        assert_eq!(
            payload.tools,
            Some(vec![
                String::from("git status"),
                String::from("git add"),
                String::from("git commit")
            ])
        );
    }

    #[test]
    fn parse_metadata_with_thinking() {
        let payload = parse_metadata_fields(&[
            String::from("name=reason"),
            String::from("thinking=on"),
            String::from("thinking_budget=2048"),
        ])
        .unwrap();
        let thinking = payload.thinking.expect("thinking config");
        assert!(thinking.enabled);
        assert_eq!(thinking.budget_tokens, Some(2048));
    }

    #[test]
    fn parse_metadata_requires_name() {
        assert!(parse_metadata_fields(&[String::from("description=desc")]).is_err());
    }
}
