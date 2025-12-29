use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    Config, MCP_SOCKET_ENV, McpAction, McpCallToolPayload, McpRequest, McpResponse, McpServerInfo,
    McpToolInfo, SessionSocketClient, default_config_dir, error::Result,
};
use clap::{Arg, Command, error::ErrorKind};
use snafu::prelude::*;

struct McpParsedArgs {
    server: Option<String>,
    tool: Option<String>,
}

pub async fn handle_mcp(parent_command: &str, command_name: &str, args: Vec<String>) -> Result<()> {
    let client = if let Some(client) = SessionSocketClient::from_mcp_env()
        .await
        .whatever_context(format!("failed to new from env {MCP_SOCKET_ENV}"))?
    {
        client
    } else if let Some(client) = connect_from_default_config().await? {
        client
    } else {
        whatever!(
            "{MCP_SOCKET_ENV} is not set. MCP might be disabled or not configured. Configure [mcp] in config.toml."
        );
    };

    let server_list = fetch_server_list(&client).await?;
    let server_names = server_list
        .iter()
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();
    let parsed = match parse_mcp_args(parent_command, command_name, &args, &server_names) {
        Ok(parsed) => parsed,
        Err(err) => {
            match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::MissingRequiredArgument
                | ErrorKind::MissingSubcommand => {
                    if let Some(server) = detect_target_server(&args, &server_names) {
                        let tools = fetch_tool_list(&client, &server).await?;
                        return emit_mcp_parser_output_with_tools(
                            parent_command,
                            command_name,
                            &args,
                            &server_names,
                            &tools,
                        );
                    }
                    err.exit();
                }
                _ => (),
            }
            whatever!("{err}");
        }
    };

    let Some(server) = parsed.server else {
        whatever!("server is required");
    };
    let Some(tool) = parsed.tool else {
        whatever!("tool is required");
    };
    let payload = McpCallToolPayload {
        tool: tool.clone(),
        arguments: None,
    };
    let request = McpRequest {
        request_id: new_request_id(),
        server: Some(server.clone()),
        action: McpAction::CallTool,
        payload: Some(
            serde_json::to_value(payload).whatever_context("failed to serialize tool payload")?,
        ),
        timeout_ms: None,
    };
    let response = client
        .send_mcp_request(request)
        .await
        .whatever_context("failed to send mcp request")?;
    let result = expect_ok(response)?;
    let output = serde_json::to_string(&result).whatever_context("failed to serialize result")?;
    println!("{output}");
    Ok(())
}

fn parse_mcp_args(
    parent_command: &str,
    command_name: &str,
    args: &[String],
    server_names: &[String],
) -> std::result::Result<McpParsedArgs, clap::Error> {
    let argv = build_mcp_argv(command_name, args);
    let matches = build_mcp_parser(parent_command, command_name, server_names, None)
        .try_get_matches_from(argv)?;

    Ok(McpParsedArgs {
        server: matches.get_one::<String>("server").cloned(),
        tool: matches.get_one::<String>("tool").cloned(),
    })
}

fn build_mcp_parser(
    parent_command: &str,
    command_name: &str,
    server_names: &[String],
    tools: Option<&[McpToolInfo]>,
) -> Command {
    let mut cmd = Command::new("mcp").bin_name(format!("{parent_command} {command_name}"));

    let help_message = format!("Target MCP server name in [{}]", server_names.join(", "));
    let server_names = server_names.to_owned();
    let choice = move |src: &str| -> Result<String> {
        server_names
            .iter()
            .find(|name| src == name.as_str())
            .map(|_| src.to_string())
            .whatever_context("target MCP server not configured")
    };
    let server_arg = Arg::new("server")
        .value_parser(choice)
        .required(true)
        .allow_hyphen_values(true)
        .help(help_message);
    cmd = cmd.arg(server_arg);

    let tool_arg = if let Some(tools) = tools {
        let help_message = format!(
            "Target MCP tool name in [{}]",
            tools
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let tools = tools.to_owned();
        let choice = move |src: &str| -> Result<McpToolInfo> {
            tools
                .iter()
                .find(|tool| src == tool.name)
                .cloned()
                .whatever_context("target MCP tool not exists")
        };
        Arg::new("tool")
            .value_parser(choice)
            .required(true)
            .allow_hyphen_values(true)
            .help(help_message)
    } else {
        Arg::new("tool")
            .required(true)
            .allow_hyphen_values(true)
            .help("Target MCP tool name")
    };
    cmd = cmd.arg(tool_arg);

    cmd
}

async fn fetch_server_list(client: &SessionSocketClient) -> Result<Vec<McpServerInfo>> {
    let request = McpRequest {
        request_id: new_request_id(),
        server: None,
        action: McpAction::ListServers,
        payload: None,
        timeout_ms: None,
    };
    let response = client
        .send_mcp_request(request)
        .await
        .whatever_context("failed to request server list")?;
    let result = expect_ok(response)?;
    serde_json::from_value(result).whatever_context("invalid server list")
}

async fn fetch_tool_list(client: &SessionSocketClient, server: &str) -> Result<Vec<McpToolInfo>> {
    let request = McpRequest {
        request_id: new_request_id(),
        server: Some(server.to_string()),
        action: McpAction::ListTools,
        payload: None,
        timeout_ms: None,
    };
    let response = client
        .send_mcp_request(request)
        .await
        .whatever_context("failed to request tool list")?;
    let result = expect_ok(response)?;
    serde_json::from_value(result).whatever_context("invalid tool list")
}

fn expect_ok(response: McpResponse) -> Result<serde_json::Value> {
    if response.ok {
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    } else {
        let message = response
            .error
            .map(|err| err.message)
            .unwrap_or_else(|| "unknown mcp error".to_string());
        whatever!("{message}");
    }
}

fn detect_target_server(args: &[String], server_names: &[String]) -> Option<String> {
    args.iter()
        .find(|arg| server_names.iter().any(|name| name == *arg))
        .cloned()
}

fn build_mcp_argv(command_name: &str, args: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(command_name.to_string());
    argv.extend(args.iter().cloned());
    argv
}

fn emit_mcp_parser_output_with_tools(
    parent_command: &str,
    command_name: &str,
    args: &[String],
    server_names: &[String],
    tools: &[McpToolInfo],
) -> Result<()> {
    let argv = build_mcp_argv(command_name, args);
    let cmd = build_mcp_parser(parent_command, command_name, server_names, Some(tools));
    match cmd.try_get_matches_from(argv) {
        Ok(_) => Ok(()),
        Err(err) => err.exit(),
    }
}

fn new_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("mcp-{nanos}")
}

async fn connect_from_default_config() -> Result<Option<SessionSocketClient>> {
    let config_dir = default_config_dir();
    let config_path = config_dir.join("config.toml");
    if !config_path.exists() {
        return Ok(None);
    }
    let mut config = Config::parse_file(config_path.to_string_lossy().as_ref())
        .whatever_context("failed to parse config file")?;
    config.config_dir = config_dir;
    let Some(mcp) = config.mcp else {
        return Ok(None);
    };
    let socket_path = mcp.resolved_socket_path(&config.config_dir);
    let client = SessionSocketClient::connect(socket_path)
        .await
        .whatever_context("failed to connect to mcp socket")?;
    Ok(Some(client))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_help_includes_server_list() {
        let parent_command = "coco";
        let command_name = "mcp";
        let server_names = vec!["alpha".to_string(), "beta".to_string()];
        let expected = "Target MCP server name in [alpha, beta]";
        let help_args = ["-h", "--help"];

        for arg in help_args {
            let err = build_mcp_parser(parent_command, command_name, &server_names, None)
                .try_get_matches_from(vec![command_name, arg])
                .expect_err("expected help to exit");
            assert_eq!(err.kind(), ErrorKind::DisplayHelp);
            assert!(
                err.to_string().contains(expected),
                "help output missing server list"
            );
        }
    }

    #[test]
    fn test_mcp_help_includes_tool_list() {
        let parent_command = "coco";
        let command_name = "mcp";
        let server_names = vec!["alpha".to_string()];
        let tools = vec![
            McpToolInfo {
                name: "read".to_string(),
                description: Some("Read tool".to_string()),
                input_schema: serde_json::json!({}),
            },
            McpToolInfo {
                name: "write".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            },
        ];

        let err = build_mcp_parser(parent_command, command_name, &server_names, Some(&tools))
            .try_get_matches_from(vec![command_name, "alpha", "--help"])
            .expect_err("expected help to exit");
        let output = err.to_string();
        assert!(output.contains("read"), "help output missing tool name");
        assert!(output.contains("write"), "help output missing tool name");
    }

    #[test]
    fn test_mcp_requires_tool() {
        let parent_command = "coco";
        let command_name = "mcp";
        let server_names = vec!["alpha".to_string()];

        let err = build_mcp_parser(parent_command, command_name, &server_names, None)
            .try_get_matches_from(vec![command_name, "alpha"])
            .expect_err("expected tool to be required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }
}
