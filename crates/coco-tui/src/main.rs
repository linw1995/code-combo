use std::path::PathBuf;

use clap::{Parser, Subcommand};
use code_combo::{
    Config,
    cli::{ClientCommand, handle_client_command, init_client_logging},
    default_config_dir,
};
use snafu::prelude::*;

use coco_tui::{
    actions::{Action, ComboAction},
    app,
    components::Chat,
    error::Result,
    global, version,
};
use tracing::info;

/// Code Combo
#[derive(Parser)]
#[command(name="coco", version, long_version=version::long_versions(), about)]
struct Args {
    /// Config file path
    #[arg(long)]
    config_path: Option<String>,

    /// Config file dir
    #[arg(long, default_value_t = default_config_dir().to_string_lossy().to_string())]
    config_dir: String,

    /// Restore the last session
    #[arg(short = 'r', long)]
    restore: bool,

    /// Prompt text to submit after TUI starts
    #[arg(trailing_var_arg = true, value_name = "prompt")]
    prompt: Vec<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Clone)]
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
    #[command(subcommand)]
    Combo(ComboCommands),
}

#[derive(Subcommand, Clone)]
enum ComboCommands {
    Run { name: String },
}

impl TryFrom<Commands> for ClientCommand {
    type Error = Commands;

    fn try_from(value: Commands) -> std::result::Result<Self, Self::Error> {
        match value {
            Commands::Metadata { fields } => Ok(ClientCommand::Metadata { fields }),
            Commands::Ask { prompt } => Ok(ClientCommand::Ask { prompt }),
            Commands::Record {
                wrap_result,
                command,
            } => Ok(ClientCommand::Record {
                wrap_result,
                command,
            }),
            _ => Err(value),
        }
    }
}

#[snafu::report]
#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    if !args.prompt.is_empty() {
        ensure_whatever!(
            args.command.is_none(),
            "prompt cannot be used with subcommands"
        );
        ensure_whatever!(!args.restore, "prompt cannot be used with --restore");
    }
    let startup_prompt = if args.prompt.is_empty() {
        None
    } else {
        let prompt = args.prompt.join(" ");
        ensure_whatever!(!prompt.trim().is_empty(), "prompt is required");
        Some(prompt)
    };
    if let Some(command) = args.command.take() {
        match ClientCommand::try_from(command) {
            Ok(command) => {
                init_client_logging("coco", &command);
                return handle_client_command(command)
                    .await
                    .whatever_context("failed to handle client command");
            }
            Err(command) => {
                args.command.replace(command);
            }
        }
    }

    coco_tui::logging::init()?;

    let config_dir: PathBuf = args.config_dir.parse().expect("Invalid config dir");

    if args.config_path.is_none() {
        args.config_path
            .replace(config_dir.join("config.toml").to_string_lossy().to_string());
    }
    let mut config = Config::parse_file(&args.config_path.unwrap())
        .whatever_context("failed to parse config file")?;
    config.config_dir = config_dir;
    global::set_config(config.clone()).await;

    let mut root_view = Chat::new(config);
    root_view.setup().await;
    let mut app = app::App::new(Box::new(root_view))?;
    if let Some(prompt) = startup_prompt {
        app.send_action(Action::SubmitPrompt(prompt));
    }
    match args.command {
        Some(Commands::Combo(combo_cmd)) => match combo_cmd {
            ComboCommands::Run { name } => {
                app.send_action(ComboAction::Execute { name }.into());
            }
        },
        Some(Commands::Metadata { .. } | Commands::Ask { .. } | Commands::Record { .. }) => {
            panic!("combo command should have been handled earlier");
        }
        None => {
            if args.restore {
                info!("restoring last session");
                app.send_action(Action::restore_last_session());
            }
        }
    }

    let result = app.run().await;

    ratatui::restore();

    result
}
