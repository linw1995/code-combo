use std::{path::PathBuf, sync::OnceLock};

use clap::{Parser, Subcommand};
use code_combo::{Config, default_config_dir};
use snafu::prelude::*;

use coco_tui::{
    actions::{Action, ComboAction},
    app,
    error::Result,
    global,
};
use tracing::info;

static LONG_VERSION: OnceLock<String> = OnceLock::new();

fn long_version() -> &'static str {
    LONG_VERSION
        .get_or_init(|| {
            // This closure is executed only once, on the first call to get_or_init
            let dirty = if env!("GIT_DIRTY") == "true" {
                "[dirty]"
            } else {
                ""
            };
            format!(
                "{} (sha:{:?}, build_time:{:?}){}",
                env!("CARGO_PKG_VERSION"),
                env!("GIT_COMMIT_SHA"),
                env!("BUILT_TIME_UTC"),
                dirty
            )
        })
        .as_str()
}

/// Code Combo
#[derive(Parser)]
#[command(name="coco", version, long_version=long_version(), about)]
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

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(subcommand)]
    Combo(ComboCommands),
}

#[derive(Subcommand)]
enum ComboCommands {
    Run { name: String },
}

#[snafu::report]
#[tokio::main]
async fn main() -> Result<()> {
    coco_tui::logging::init()?;

    let mut args = Args::parse();
    let config_dir: PathBuf = args.config_dir.parse().expect("Invalid config dir");

    if args.config_path.is_none() {
        args.config_path
            .replace(config_dir.join("config.toml").to_string_lossy().to_string());
    }
    let mut config = Config::parse_file(&args.config_path.unwrap())
        .whatever_context("failed to parse config file")?;
    config.config_dir = config_dir;
    global::set_config(config.clone()).await;

    let mut app = app::App::new(config)?;
    match args.command {
        Some(Commands::Combo(combo_cmd)) => match combo_cmd {
            ComboCommands::Run { name } => {
                app.send_action(ComboAction::Execute { name }.into());
            }
        },
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
