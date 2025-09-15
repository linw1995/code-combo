use clap::{Parser, Subcommand};
use code_combo::Config;

/// Code Combo
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Config file path
    #[arg(short, long, default_value_t = default_config_path())]
    config_path: String,
    #[command(subcommand)]
    command: Option<Commands>,
}

fn default_config_path() -> String {
    if let Some(dir) = dirs::config_dir() {
        dir.join("code-combo")
            .join("config.toml")
            .to_string_lossy()
            .to_string()
    } else {
        "config.toml".to_string()
    }
}

#[derive(Subcommand)]
enum Commands {
    #[command(subcommand)]
    Combo(ComboCommands),
}

#[derive(Subcommand)]
enum ComboCommands {
    List,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = Config::parse_file(&args.config_path);
    println!("{:?}", config);
}
