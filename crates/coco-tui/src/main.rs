use std::path::PathBuf;

use clap::{Parser, Subcommand};
use code_combo::{Config, default_config_dir};
use snafu::prelude::*;

use coco_tui::{actions::ComboAction, app, error::Result, global};

/// Code Combo
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Config file path
    #[arg(long)]
    config_path: Option<String>,

    /// Config file dir
    #[arg(long, default_value_t = default_config_dir().to_string_lossy().to_string())]
    config_dir: String,

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
        None => {}
    }

    let result = app.run().await;

    ratatui::restore();

    result
}
