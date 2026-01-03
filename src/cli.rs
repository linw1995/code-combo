use snafu::prelude::*;

use crate::error::Result;

#[derive(Debug, Clone)]
pub enum ClientCommand {
    Metadata {
        fields: Vec<String>,
    },
    Ask {
        prompt: Vec<String>,
        reply: bool,
        schemas: Vec<String>,
    },
    Record {
        wrap_result: bool,
        command: Vec<String>,
    },
    Mcp {
        args: Vec<String>,
    },
}

pub fn init_client_logging(program: &str, command: &ClientCommand) {
    let sub = match command {
        ClientCommand::Metadata { .. } => "metadata",
        ClientCommand::Ask { .. } => "ask",
        ClientCommand::Record { .. } => "record",
        ClientCommand::Mcp { .. } => "mcp",
    };
    let log_name = format!("{program}-{sub}");
    let _ = crate::logging::init_file_logging_best_effort(&log_name);
}

pub async fn handle_client_command(
    parent_command: &str,
    command_name: &str,
    command: ClientCommand,
) -> Result<()> {
    match command {
        ClientCommand::Metadata { fields } => crate::cmd::handle_metadata(fields)
            .await
            .whatever_context("failed to handle metadata"),
        ClientCommand::Ask {
            prompt,
            reply,
            schemas,
        } => crate::cmd::handle_ask(prompt.join(" "), reply, schemas)
            .await
            .whatever_context("failed to handle ask"),
        ClientCommand::Record {
            wrap_result,
            command,
        } => crate::cmd::handle_record(wrap_result, command)
            .await
            .whatever_context("failed to handle record"),
        ClientCommand::Mcp { args } => crate::cmd::handle_mcp(parent_command, command_name, args)
            .await
            .whatever_context("failed to handle mcp"),
    }
}
