use std::path::PathBuf;

use clap::{Parser, Subcommand};
use code_combo::Config;
use color_eyre::eyre::eyre;

use coco_tui::{actions::ComboAction, app, global};

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

fn default_config_dir() -> PathBuf {
    PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME environment variable not set");
        format!("{}/.config", home)
    }))
    .join("coco")
}

#[derive(Subcommand)]
enum Commands {
    #[command(subcommand)]
    Combo(ComboCommands),
}

#[derive(Subcommand)]
enum ComboCommands {
    List,
    Run { name: String },
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    coco_tui::logging::init()?;

    let mut args = Args::parse();
    let config_dir: PathBuf = args.config_dir.parse().expect("Invalid config dir");

    if args.config_path.is_none() {
        args.config_path
            .replace(config_dir.join("config.toml").to_string_lossy().to_string());
    }
    let mut config = Config::parse_file(&args.config_path.unwrap())
        .map_err(|err| eyre!("parse file error: {err}"))?;
    config.config_dir = config_dir;
    global::set_config(config.clone()).await;

    let mut app = app::App::new(config)?;
    match args.command {
        Some(Commands::Combo(combo_cmd)) => match combo_cmd {
            ComboCommands::List => {
                app.send_action(ComboAction::Discover.into());
            }
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
