use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use code_combo::{
    cli::{ClientCommand, handle_client_command, init_client_logging},
    version,
};
use snafu::prelude::*;

/// Code Combo client
#[derive(Debug, Parser)]
#[command(name = "coco", version, long_version = version::long_version(), about)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Metadata {
        /// Metadata entries in key=value format (name required)
        #[arg(value_name = "key=value")]
        fields: Vec<String>,
    },
    Ask {
        /// Ask the model to reply via a tool call
        #[arg(long, requires = "schemas")]
        reply: bool,
        /// Response schemas in field:description format (repeatable)
        #[arg(long, value_name = "field:description")]
        schemas: Vec<String>,
        /// Prompt text to send via session socket (or read from stdin when omitted)
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    Record {
        /// Capture and emit wrapped JSON result
        #[arg(long)]
        wrap_result: bool,

        /// Command to execute and record
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Mcp {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[snafu::report]
#[tokio::main]
async fn main() -> code_combo::Result<()> {
    let cmd = Args::command();
    let program = cmd.get_name().to_string();
    let matches = cmd.get_matches();
    let command_name = matches.subcommand_name().unwrap_or("coco").to_string();
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());
    let command = match args.command {
        Commands::Metadata { fields } => ClientCommand::Metadata { fields },
        Commands::Ask {
            prompt,
            reply,
            schemas,
        } => ClientCommand::Ask {
            prompt,
            reply,
            schemas,
        },
        Commands::Record {
            wrap_result,
            command,
        } => ClientCommand::Record {
            wrap_result,
            command,
        },
        Commands::Mcp { args } => ClientCommand::Mcp { args },
    };

    init_client_logging(&program, &command);
    handle_client_command(&program, &command_name, command)
        .await
        .whatever_context("failed to handle client command")
}
