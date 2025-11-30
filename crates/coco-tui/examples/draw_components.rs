use std::{io::stderr, path::PathBuf};

use clap::{Parser, Subcommand};
use coco_tui::{
    components::{Chat, CodeHighlight, Component},
    error::Result,
    global,
    session::{self, Session},
};
use code_combo::{Config, default_config_dir};
use crossterm::{
    cursor,
    event::{EventStream, KeyCode, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use indoc::indoc;
use ratatui::{Terminal, prelude::CrosstermBackend};
use snafu::prelude::*;

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
    CodeHighlight,

    Session { path: String },
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

    // Initialize dummy channels for testing.
    use tokio::sync::mpsc;
    let (event_tx, _) = mpsc::unbounded_channel();
    let (action_tx, _) = mpsc::unbounded_channel();
    global::initialize(event_tx, action_tx);

    let mut component: Box<dyn Component> = match args.command {
        Some(Commands::CodeHighlight) => {
            let app = CodeHighlight::try_new(
                indoc! {"
            @@ -1,3 +1,4 @@
             line 1
            -line 2
            +line 2 modified
             line 3
            +line 4
        "}
                .trim(),
                code_highlight::Lang::Diff,
            )
            .whatever_context("failed to new CodeHighlight")?;
            Box::new(app)
        }
        Some(Commands::Session { path }) => {
            let content = tokio::fs::read_to_string(path)
                .await
                .whatever_context("failed to read text file")?;
            let s: Session =
                serde_json::from_str(&content).whatever_context("failed to deserialize json")?;
            let (type_id, s): (String, Session) = session::load_related(s)?;
            session::load_component(&type_id, s)?
        }
        None => Box::new(Chat::new(config)),
    };

    let backend = CrosstermBackend::new(stderr());
    let mut terminal = Terminal::new(backend).whatever_context("failed to new terminal")?;
    let mut event_stream = EventStream::new();

    crossterm::terminal::enable_raw_mode().whatever_context("failed to enable raw mode")?;
    crossterm::execute!(stderr(), EnterAlternateScreen, cursor::Hide)
        .whatever_context("failed to enter alter screen")?;

    terminal
        .draw(|frame| {
            component.draw(frame, frame.area()).expect("failed to draw");
        })
        .whatever_context("failed to draw")?;

    loop {
        let crossterm_event = event_stream.next().fuse().await;
        if let Some(Ok(crossterm::event::Event::Key(key))) = crossterm_event
            && key.code == KeyCode::Char('c')
            && key.modifiers == KeyModifiers::CONTROL
        {
            break;
        }
    }

    if crossterm::terminal::is_raw_mode_enabled()
        .whatever_context("failed to check raw mode enabled")?
    {
        terminal
            .flush()
            .whatever_context("failed to flush terminal")?;
        crossterm::execute!(stderr(), LeaveAlternateScreen, cursor::Show)
            .whatever_context("faile to leave alter screen")?;
        crossterm::terminal::disable_raw_mode().whatever_context("failed to disable raw mode")?;
    }

    Ok(())
}
