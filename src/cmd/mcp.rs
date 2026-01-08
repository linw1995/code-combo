use std::{
    collections::HashSet,
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    MCP_SOCKET_ENV, McpAction, McpCallToolPayload, McpRequest, McpResponse, McpServerInfo,
    McpToolInfo, SessionSocketClient, default_config_dir, error::Result,
    load_config_with_overrides, workspace_config_path,
};
use clap::{
    Arg, ArgAction, Command, builder::PossibleValuesParser, error::ErrorKind, value_parser,
};
use rmcp::model::CallToolResult;
use serde_json::{Map, Number, Value};
use snafu::prelude::*;

struct McpParsedArgs {
    server: String,
    tool: String,
    arguments: Option<Value>,
}

const TOOL_ARGS_JSON: &str = "__tool_args_json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SchemaValueKind {
    String,
    Integer,
    Number,
    Boolean,
    Json,
}

#[derive(Clone, Debug)]
struct ToolArgSpec {
    name: String,
    required: bool,
    kind: SchemaValueKind,
    is_array: bool,
    enum_values: Option<Vec<String>>,
    description: Option<String>,
}

impl ToolArgSpec {
    fn value_name(&self) -> &'static str {
        match self.kind {
            SchemaValueKind::String => "STRING",
            SchemaValueKind::Integer => "INT",
            SchemaValueKind::Number => "NUMBER",
            SchemaValueKind::Boolean => "BOOL",
            SchemaValueKind::Json => "JSON",
        }
    }
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
    let parsed = if let Some(server) = detect_target_server(&args, &server_names) {
        let tools = fetch_tool_list(&client, &server).await?;
        let argv = build_mcp_argv(command_name, &args);
        let cmd = build_mcp_parser(parent_command, command_name, &server_names, Some(&tools));
        let matches = match cmd.try_get_matches_from(argv) {
            Ok(matches) => matches,
            Err(err) => match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::MissingRequiredArgument
                | ErrorKind::MissingSubcommand => err.exit(),
                _ => whatever!("{err}"),
            },
        };
        build_parsed_args(&matches, &tools)?
    } else {
        let argv = build_mcp_argv(command_name, &args);
        let cmd = build_mcp_parser(parent_command, command_name, &server_names, None);
        match cmd.try_get_matches_from(argv) {
            Ok(_) => whatever!("server is required"),
            Err(err) => match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::MissingRequiredArgument
                | ErrorKind::MissingSubcommand => err.exit(),
                _ => whatever!("{err}"),
            },
        }
    };

    let server = parsed.server;
    let tool = parsed.tool;
    let payload = McpCallToolPayload {
        tool: tool.clone(),
        arguments: parsed.arguments,
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
    let output = render_final_output(&result)?;
    println!("{output}");
    Ok(())
}

fn build_mcp_parser(
    parent_command: &str,
    command_name: &str,
    server_names: &[String],
    tools: Option<&[McpToolInfo]>,
) -> Command {
    let mut cmd = Command::new("mcp")
        .bin_name(format!("{parent_command} {command_name}"))
        .disable_help_subcommand(true);

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

    if let Some(tools) = tools {
        let mut tools = tools.to_vec();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        cmd = cmd
            .subcommand_required(true)
            .subcommand_value_name("tool")
            .subcommand_help_heading("Tools");
        for tool in &tools {
            cmd = cmd.subcommand(build_tool_subcommand(tool));
        }
    }

    cmd
}

fn build_tool_subcommand(tool: &McpToolInfo) -> Command {
    let mut cmd = Command::new(tool.name.clone());
    if let Some(description) = &tool.description {
        cmd = cmd.about(description.clone());
    }

    let specs = tool_arg_specs(&tool.input_schema);
    let mut arg_names = Vec::new();
    for spec in specs {
        let mut arg = Arg::new(spec.name.clone())
            .long(spec.name.clone())
            .value_name(spec.value_name());
        if let Some(description) = spec.description {
            arg = arg.help(description);
        }

        arg = match spec.kind {
            SchemaValueKind::String | SchemaValueKind::Json => arg,
            SchemaValueKind::Integer => arg.value_parser(value_parser!(i64)),
            SchemaValueKind::Number => arg.value_parser(value_parser!(f64)),
            SchemaValueKind::Boolean => arg.value_parser(value_parser!(bool)),
        };

        if let Some(values) = &spec.enum_values {
            arg = arg.value_parser(PossibleValuesParser::new(values.clone()));
        }

        if spec.is_array {
            arg = arg.action(ArgAction::Append);
        }

        if spec.required {
            arg = arg.required_unless_present(TOOL_ARGS_JSON);
        }

        arg_names.push(spec.name.clone());
        cmd = cmd.arg(arg);
    }

    let mut args_json = Arg::new(TOOL_ARGS_JSON)
        .long("args-json")
        .value_name("JSON")
        .help("JSON-encoded tool arguments");
    if !arg_names.is_empty() {
        args_json = args_json.conflicts_with_all(arg_names.iter().cloned());
    }
    cmd.arg(args_json)
}

fn build_parsed_args(matches: &clap::ArgMatches, tools: &[McpToolInfo]) -> Result<McpParsedArgs> {
    let server = matches
        .get_one::<String>("server")
        .cloned()
        .whatever_context("server is required")?;
    let (tool_name, tool_matches) = matches.subcommand().whatever_context("tool is required")?;
    let tool = tools
        .iter()
        .find(|tool| tool.name == tool_name)
        .whatever_context("target MCP tool not exists")?;
    let arguments = build_tool_arguments(tool, tool_matches)?;
    Ok(McpParsedArgs {
        server,
        tool: tool.name.clone(),
        arguments,
    })
}

fn tool_arg_specs(schema: &Value) -> Vec<ToolArgSpec> {
    let Some(schema_obj) = schema.as_object() else {
        return Vec::new();
    };
    let properties = schema_obj
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if properties.is_empty() {
        return Vec::new();
    }

    let required = schema_obj
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();

    let mut names = properties.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let schema = properties.get(&name)?;
            let description = schema
                .get("description")
                .and_then(Value::as_str)
                .map(|value| value.to_string());
            let (kind, is_array, enum_values) = parse_property_schema(schema);
            let is_required = required.contains(&name);
            Some(ToolArgSpec {
                name,
                required: is_required,
                kind,
                is_array,
                enum_values,
                description,
            })
        })
        .collect()
}

fn parse_property_schema(schema: &Value) -> (SchemaValueKind, bool, Option<Vec<String>>) {
    let schema_type = schema.get("type");
    if schema_type
        .and_then(Value::as_str)
        .map(|value| value == "array")
        .unwrap_or(false)
    {
        let items = schema.get("items").unwrap_or(&Value::Null);
        let (kind, enum_values) = parse_scalar_schema(items);
        return (kind, true, enum_values);
    }

    let (kind, enum_values) = parse_scalar_schema(schema);
    (kind, false, enum_values)
}

fn parse_scalar_schema(schema: &Value) -> (SchemaValueKind, Option<Vec<String>>) {
    if let Some(types) = schema.get("type").and_then(Value::as_array)
        && let Some(kind) = types
            .iter()
            .filter_map(Value::as_str)
            .find_map(schema_type_to_kind)
    {
        return (kind, extract_string_enum(schema, kind));
    }

    if let Some(schema_type) = schema.get("type").and_then(Value::as_str) {
        let kind = schema_type_to_kind(schema_type).unwrap_or(SchemaValueKind::Json);
        return (kind, extract_string_enum(schema, kind));
    }

    let kind = if schema.get("enum").is_some() {
        SchemaValueKind::String
    } else {
        SchemaValueKind::Json
    };
    (kind, extract_string_enum(schema, kind))
}

fn extract_string_enum(schema: &Value, kind: SchemaValueKind) -> Option<Vec<String>> {
    if kind != SchemaValueKind::String {
        return None;
    }
    schema.get("enum").and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    })
}

fn schema_type_to_kind(value: &str) -> Option<SchemaValueKind> {
    match value {
        "string" => Some(SchemaValueKind::String),
        "integer" => Some(SchemaValueKind::Integer),
        "number" => Some(SchemaValueKind::Number),
        "boolean" => Some(SchemaValueKind::Boolean),
        "object" => Some(SchemaValueKind::Json),
        _ => None,
    }
}

fn build_tool_arguments(tool: &McpToolInfo, matches: &clap::ArgMatches) -> Result<Option<Value>> {
    if let Some(raw) = matches.get_one::<String>(TOOL_ARGS_JSON) {
        let raw = if raw == "-" {
            read_args_json_from_stdin()?
        } else {
            raw.clone()
        };
        let value =
            serde_json::from_str(&raw).whatever_context("failed to parse args-json argument")?;
        return Ok(Some(value));
    }

    let specs = tool_arg_specs(&tool.input_schema);
    if specs.is_empty() {
        return Ok(None);
    }

    let mut arguments = Map::new();
    for spec in specs {
        if spec.is_array {
            if let Some(values) = collect_array_values(matches, &spec)? {
                arguments.insert(spec.name, Value::Array(values));
            }
        } else if let Some(value) = collect_single_value(matches, &spec)? {
            arguments.insert(spec.name, value);
        }
    }

    if arguments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(arguments)))
    }
}

fn read_args_json_from_stdin() -> Result<String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .whatever_context("failed to read args-json from stdin")?;
    Ok(input)
}

fn collect_single_value(matches: &clap::ArgMatches, spec: &ToolArgSpec) -> Result<Option<Value>> {
    match spec.kind {
        SchemaValueKind::String => Ok(matches
            .get_one::<String>(&spec.name)
            .map(|value| Value::String(value.clone()))),
        SchemaValueKind::Integer => Ok(matches
            .get_one::<i64>(&spec.name)
            .map(|value| Value::Number(Number::from(*value)))),
        SchemaValueKind::Number => matches
            .get_one::<f64>(&spec.name)
            .map(|value| number_value_from_f64(*value, &spec.name))
            .transpose(),
        SchemaValueKind::Boolean => Ok(matches
            .get_one::<bool>(&spec.name)
            .map(|value| Value::Bool(*value))),
        SchemaValueKind::Json => matches
            .get_one::<String>(&spec.name)
            .map(|value| json_value_from_str(value, &spec.name))
            .transpose(),
    }
}

fn collect_array_values(
    matches: &clap::ArgMatches,
    spec: &ToolArgSpec,
) -> Result<Option<Vec<Value>>> {
    match spec.kind {
        SchemaValueKind::String => Ok(matches
            .get_many::<String>(&spec.name)
            .map(|values| values.map(|value| Value::String(value.clone())).collect())),
        SchemaValueKind::Integer => Ok(matches.get_many::<i64>(&spec.name).map(|values| {
            values
                .map(|value| Value::Number(Number::from(*value)))
                .collect()
        })),
        SchemaValueKind::Number => matches
            .get_many::<f64>(&spec.name)
            .map(|values| {
                values
                    .map(|value| number_value_from_f64(*value, &spec.name))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose(),
        SchemaValueKind::Boolean => Ok(matches
            .get_many::<bool>(&spec.name)
            .map(|values| values.map(|value| Value::Bool(*value)).collect())),
        SchemaValueKind::Json => matches
            .get_many::<String>(&spec.name)
            .map(|values| {
                values
                    .map(|value| json_value_from_str(value, &spec.name))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose(),
    }
}

fn number_value_from_f64(value: f64, name: &str) -> Result<Value> {
    Number::from_f64(value)
        .map(Value::Number)
        .whatever_context(format!("failed to parse JSON number for {name}"))
}

fn json_value_from_str(value: &str, name: &str) -> Result<Value> {
    serde_json::from_str::<Value>(value)
        .whatever_context(format!("failed to parse JSON value for {name}"))
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

fn render_final_output(result: &Value) -> Result<String> {
    let parsed = serde_json::from_value::<CallToolResult>(result.clone());
    if let Ok(call_result) = parsed {
        if let Some(structured) = call_result.structured_content {
            return serde_json::to_string(&structured)
                .whatever_context("failed to serialize structured content");
        }
        if !call_result.content.is_empty() {
            let mut parts = Vec::with_capacity(call_result.content.len());
            for content in call_result.content {
                if let Some(text) = content.as_text() {
                    parts.push(text.text.clone());
                } else {
                    return serde_json::to_string(result)
                        .whatever_context("failed to serialize result");
                }
            }
            return Ok(parts.join("\n"));
        }
    }

    serde_json::to_string(result).whatever_context("failed to serialize result")
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
    let workspace_path = workspace_config_path();
    let config = load_config_with_overrides(&config_path, &config_dir, Some(&workspace_path))
        .whatever_context("failed to parse config file")?;
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
    use std::{env, path::PathBuf, sync::Arc};

    use indoc::indoc;
    use rmcp::{model::Content, service::ServiceExt, transport::StreamableHttpClientTransport};
    use snafu::prelude::*;
    use tokio::process::Command;

    use crate::mcp::tests::{TestDropGuard, TestHttpServer, TestMcpSocketServer, install_peer};
    use crate::{
        McpConfig, McpManager, McpServerCommandConfig, McpServerConfig, McpServerConnection,
        SessionSocketClient,
    };

    use super::*;

    fn resolve_coco_bin() -> Result<PathBuf> {
        let target_dir = env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
        let mut path = target_dir.join(profile).join("coco");
        if cfg!(windows) {
            path.set_extension("exe");
        }
        ensure_whatever!(path.exists(), "coco binary not found at {path:?}");
        Ok(path)
    }

    async fn run_coco_mcp(
        socket_path: &std::path::Path,
        server: &str,
        tool: &str,
        args: &[&str],
    ) -> Result<String> {
        let coco_bin = resolve_coco_bin()?;
        let output = Command::new(coco_bin)
            .arg("mcp")
            .arg(server)
            .arg(tool)
            .args(args)
            .env(MCP_SOCKET_ENV, socket_path)
            .output()
            .await
            .whatever_context("failed to execute coco mcp")?;
        ensure_whatever!(
            output.status.success(),
            "coco mcp failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).whatever_context("invalid stdout from coco mcp")
    }

    struct ExpectedOutput {
        stdout: &'static str,
        stderr: &'static str,
        success: bool,
    }

    async fn run_coco_mcp_command(
        socket_path: &std::path::Path,
        args: &[&str],
    ) -> Result<std::process::Output> {
        let coco_bin = resolve_coco_bin()?;
        Command::new(coco_bin)
            .arg("mcp")
            .args(args)
            .env(MCP_SOCKET_ENV, socket_path)
            .output()
            .await
            .whatever_context("failed to execute coco mcp")
    }

    async fn setup_mcp_test_env() -> Result<(TestDropGuard, PathBuf)> {
        let server_alpha = TestHttpServer::start().await?;
        let alpha_url = server_alpha.base_url().to_string();
        let mut guard = TestDropGuard::new();
        guard.add_http_server(server_alpha);

        let config = McpConfig {
            socket_path: PathBuf::from("mcp.sock"),
            request_timeout_ms: 5_000,
            idle_ttl_ms: 0,
            servers: vec![
                McpServerConfig {
                    name: "alpha".to_string(),
                    description: Some("Alpha server".to_string()),
                    connection: McpServerConnection::Command(McpServerCommandConfig {
                        command: "unused".to_string(),
                        args: Vec::new(),
                        cwd: None,
                        env: None,
                    }),
                },
                McpServerConfig {
                    name: "beta".to_string(),
                    description: Some("Beta server".to_string()),
                    connection: McpServerConnection::Command(McpServerCommandConfig {
                        command: "unused".to_string(),
                        args: Vec::new(),
                        cwd: None,
                        env: None,
                    }),
                },
            ],
        };
        let manager = Arc::new(McpManager::new(config));

        let transport_alpha = StreamableHttpClientTransport::from_uri(alpha_url);
        let service_alpha =
            ().serve(transport_alpha)
                .await
                .whatever_context("failed to connect to alpha server")?;
        install_peer(&manager, "alpha", service_alpha).await;
        guard.add_peer(manager.clone(), "alpha");

        let socket_server = TestMcpSocketServer::start(manager.clone()).await?;
        let socket_path = socket_server.socket_path().to_path_buf();
        guard.add_socket_server(socket_server);

        Ok((guard, socket_path))
    }

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
        let tools = vec![McpToolInfo {
            name: "ping".to_string(),
            description: None,
            input_schema: serde_json::json!({}),
        }];

        let err = build_mcp_parser(parent_command, command_name, &server_names, Some(&tools))
            .try_get_matches_from(vec![command_name, "alpha"])
            .expect_err("expected tool to be required");
        assert_eq!(err.kind(), ErrorKind::MissingSubcommand);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[snafu::report]
    async fn mcp_cmd_supports_multiple_servers_and_tools_list() -> Result<()> {
        let server_alpha = TestHttpServer::start().await?;
        let server_beta = TestHttpServer::start().await?;
        let alpha_url = server_alpha.base_url().to_string();
        let beta_url = server_beta.base_url().to_string();
        let mut guard = TestDropGuard::new();
        guard.add_http_server(server_alpha);
        guard.add_http_server(server_beta);
        let config = McpConfig {
            socket_path: PathBuf::from("mcp.sock"),
            request_timeout_ms: 5_000,
            idle_ttl_ms: 0,
            servers: vec![
                McpServerConfig {
                    name: "alpha".to_string(),
                    description: Some("Alpha server".to_string()),
                    connection: McpServerConnection::Command(McpServerCommandConfig {
                        command: "unused".to_string(),
                        args: Vec::new(),
                        cwd: None,
                        env: None,
                    }),
                },
                McpServerConfig {
                    name: "beta".to_string(),
                    description: Some("Beta server".to_string()),
                    connection: McpServerConnection::Command(McpServerCommandConfig {
                        command: "unused".to_string(),
                        args: Vec::new(),
                        cwd: None,
                        env: None,
                    }),
                },
            ],
        };
        let manager = Arc::new(McpManager::new(config));

        let transport_alpha = StreamableHttpClientTransport::from_uri(alpha_url);
        let service_alpha =
            ().serve(transport_alpha)
                .await
                .whatever_context("failed to connect to alpha server")?;
        install_peer(&manager, "alpha", service_alpha).await;
        guard.add_peer(manager.clone(), "alpha");

        let transport_beta = StreamableHttpClientTransport::from_uri(beta_url);
        let service_beta =
            ().serve(transport_beta)
                .await
                .whatever_context("failed to connect to beta server")?;
        install_peer(&manager, "beta", service_beta).await;
        guard.add_peer(manager.clone(), "beta");

        let socket_server = TestMcpSocketServer::start(manager.clone()).await?;
        let socket_path = socket_server.socket_path().to_path_buf();
        guard.add_socket_server(socket_server);

        let client = SessionSocketClient::connect(socket_path.as_path())
            .await
            .whatever_context("failed to connect mcp socket")?;
        let servers = fetch_server_list(&client).await?;
        assert!(
            servers.iter().any(|server| server.name == "alpha"),
            "server list should include alpha"
        );
        assert!(
            servers.iter().any(|server| server.name == "beta"),
            "server list should include beta"
        );

        let tools = fetch_tool_list(&client, "alpha").await?;
        assert!(
            tools.iter().any(|tool| tool.name == "echo"),
            "tool list should include echo"
        );
        assert!(
            tools.iter().any(|tool| tool.name == "ping"),
            "tool list should include ping"
        );

        let output = run_coco_mcp(
            socket_path.as_path(),
            "alpha",
            "echo",
            &["--message", "hello"],
        )
        .await?;
        let output = output.trim_end_matches(['\n', '\r']);
        assert_eq!(output, "echo: hello");

        guard.shutdown().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[snafu::report]
    async fn mcp_cmd_help_and_errors_for_cli() -> Result<()> {
        let (guard, socket_path) = setup_mcp_test_env().await?;
        let missing_required_server = indoc! {"
            error: the following required arguments were not provided:
            \x20\x20<server>

            Usage: coco mcp <server>

            For more information, try '--help'.
        "};
        let missing_subcommand = indoc! {"
            error: 'coco mcp' requires a subcommand but one was not provided
            \x20\x20[subcommands: echo, ping]

            Usage: coco mcp <server> <tool>

            For more information, try '--help'.
        "};
        let help_without_tools = indoc! {"
            Usage: coco mcp <server>

            Arguments:
            \x20\x20<server>  Target MCP server name in [alpha, beta]

            Options:
            \x20\x20-h, --help  Print help
        "};
        let help_with_tools = indoc! {"
            Usage: coco mcp <server> <tool>

            Tools:
            \x20\x20echo  Echo a message
            \x20\x20ping  Ping the server

            Arguments:
            \x20\x20<server>  Target MCP server name in [alpha, beta]

            Options:
            \x20\x20-h, --help  Print help
        "};
        let ping_help = indoc! {"
            Ping the server

            Usage: coco mcp <server> ping [OPTIONS]

            Options:
            \x20\x20\x20\x20\x20\x20--args-json <JSON>  JSON-encoded tool arguments
            \x20\x20-h, --help              Print help
        "};
        let invalid_server = indoc! {"
            Error: failed to handle client command

            Caused by these errors (recent errors listed first):
            \x20\x201: failed to handle mcp
            \x20\x202: error: unexpected argument 'ping' found

            Usage: coco mcp <server>

            For more information, try '--help'.


        "};
        let cases = vec![
            (
                "coco mcp",
                vec![],
                ExpectedOutput {
                    stdout: "",
                    stderr: missing_required_server,
                    success: false,
                },
            ),
            (
                "coco mcp -h",
                vec!["-h"],
                ExpectedOutput {
                    stdout: help_without_tools,
                    stderr: "",
                    success: true,
                },
            ),
            (
                "coco mcp --help",
                vec!["--help"],
                ExpectedOutput {
                    stdout: help_without_tools,
                    stderr: "",
                    success: true,
                },
            ),
            (
                "coco mcp alpha",
                vec!["alpha"],
                ExpectedOutput {
                    stdout: "",
                    stderr: missing_subcommand,
                    success: false,
                },
            ),
            (
                "coco mcp alpha -h",
                vec!["alpha", "-h"],
                ExpectedOutput {
                    stdout: help_with_tools,
                    stderr: "",
                    success: true,
                },
            ),
            (
                "coco mcp alpha --help",
                vec!["alpha", "--help"],
                ExpectedOutput {
                    stdout: help_with_tools,
                    stderr: "",
                    success: true,
                },
            ),
            (
                "coco mcp alpha ping -h",
                vec!["alpha", "ping", "-h"],
                ExpectedOutput {
                    stdout: ping_help,
                    stderr: "",
                    success: true,
                },
            ),
            (
                "coco mcp aplpha ping --help",
                vec!["aplpha", "ping", "--help"],
                ExpectedOutput {
                    stdout: "",
                    stderr: invalid_server,
                    success: false,
                },
            ),
        ];

        for (name, args, expected) in cases {
            let output = run_coco_mcp_command(socket_path.as_path(), &args).await?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            assert_eq!(
                stdout, expected.stdout,
                "{name}: stdout mismatch. stdout={stdout:?} stderr={stderr:?}"
            );
            assert_eq!(
                stderr, expected.stderr,
                "{name}: stderr mismatch. stdout={stdout:?} stderr={stderr:?}"
            );
            assert_eq!(
                output.status.success(),
                expected.success,
                "{name}: unexpected exit status {output:?}"
            );
        }

        guard.shutdown().await;
        Ok(())
    }

    #[test]
    fn render_mcp_final_outputs_text() {
        let result = CallToolResult::success(vec![Content::text("ok")]);
        let value = serde_json::to_value(result).expect("serialize CallToolResult");
        let output = render_final_output(&value).expect("render output");
        assert_eq!(output, "ok");
    }

    #[test]
    fn render_mcp_final_prefers_structured_content() {
        let result = CallToolResult::structured(serde_json::json!({"message": "ok"}));
        let value = serde_json::to_value(result).expect("serialize CallToolResult");
        let output = render_final_output(&value).expect("render output");
        assert_eq!(output, "{\"message\":\"ok\"}");
    }
}
