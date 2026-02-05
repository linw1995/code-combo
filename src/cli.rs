use snafu::prelude::*;

use crate::error::Result;

#[derive(Debug, Clone)]
pub enum ClientCommand {
    Metadata {
        fields: Vec<String>,
    },
    Ask {
        prompt: Vec<String>,
        schemas: Vec<String>,
        interactive: bool,
    },
    Tell {
        prompt: Vec<String>,
    },
    Reply {
        /// Reply fields as --field=value format
        fields: Vec<String>,
    },
    ComboRun {
        name: String,
        args: Vec<String>,
        ignore_workspace_scripts: bool,
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
        ClientCommand::Tell { .. } => "tell",
        ClientCommand::Reply { .. } => "reply",
        ClientCommand::ComboRun { .. } => "combo-run",
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
            schemas,
            interactive,
        } => crate::cmd::handle_ask(prompt.join(" "), schemas, interactive)
            .await
            .whatever_context("failed to handle ask"),
        ClientCommand::Tell { prompt } => crate::cmd::handle_tell(prompt.join(" "))
            .await
            .whatever_context("failed to handle tell"),
        ClientCommand::Reply { fields } => crate::cmd::handle_reply(fields)
            .await
            .whatever_context("failed to handle reply"),
        ClientCommand::ComboRun {
            name,
            args,
            ignore_workspace_scripts,
        } => crate::cmd::handle_combo_run(name, args, ignore_workspace_scripts)
            .await
            .whatever_context("failed to handle combo run"),
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
