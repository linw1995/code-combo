use clap::{Parser, Subcommand};
use code_combo::{
    cli::{ClientCommand, handle_client_command, init_client_logging},
    version,
};

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
}

#[snafu::report]
#[tokio::main]
async fn main() -> code_combo::Result<()> {
    let args = Args::parse();
    let command = match args.command {
        Commands::Metadata { fields } => ClientCommand::Metadata { fields },
        Commands::Ask { prompt } => ClientCommand::Ask { prompt },
        Commands::Record {
            wrap_result,
            command,
        } => ClientCommand::Record {
            wrap_result,
            command,
        },
    };

    init_client_logging("coco", &command);
    handle_client_command(command).await
}
