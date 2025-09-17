use std::path::PathBuf;

use anthropic_api::{
    Credentials,
    messages::{
        Message, MessageContent, MessageRole, MessagesBuilder, ResponseContentBlock, Tool,
        ToolChoice,
    },
};
use clap::{Parser, Subcommand};
use code_combo::{Combo, Config, Executor, Instruction, discover_combo_starters, execute_starter};
use serde_json::json;

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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = Args::parse();

    let config_dir: PathBuf = args.config_dir.parse().expect("Invalid config dir");
    let combos_dir = config_dir.join("combos").to_string_lossy().to_string();

    if args.config_path.is_none() {
        args.config_path
            .replace(config_dir.join("config.toml").to_string_lossy().to_string());
    }
    let config = Config::parse_file(&args.config_path.unwrap())?;

    match args.command {
        Some(Commands::Combo(combo_cmd)) => match combo_cmd {
            ComboCommands::List => {
                let starters = discover_combo_starters(&combos_dir).await;
                for starter in starters {
                    match &starter.combo {
                        Ok(combo) => {
                            println!("Combo: {}", combo.metadata.name);
                            if !combo.metadata.description.is_empty() {
                                println!("  Description: {}", combo.metadata.description);
                            }
                            println!("  Mode: {}", combo.metadata.mode);
                            println!();
                        }
                        Err(err) => {
                            println!("Failed to load combo from {}: {:?}", starter.path, err);
                        }
                    }
                }
            }
            ComboCommands::Run { name } => {
                let starters = discover_combo_starters(&combos_dir).await;
                if let Some(starter) = starters.into_iter().find(|s| {
                    if let Ok(combo) = &s.combo {
                        combo.metadata.name == name
                    } else {
                        false
                    }
                }) {
                    println!("Executing combo '{}' from {}...", name, starter.path);
                    let starter = execute_starter(&starter.path, true).await;
                    run_agent(&starter.combo.unwrap(), &config).await;
                } else {
                    eprintln!("Combo '{}' not found.", name);
                };
            }
        },
        None => {
            println!("No command provided. Use --help for more information.");
        }
    }
    Ok(())
}

async fn run_agent(combo: &Combo, config: &Config) {
    let provider = config.providers.first().unwrap();
    let model = &provider.name;
    let credentials = Credentials::new(provider.api_key.clone(), provider.base_url.clone());

    let bash_tool = Tool {
        name: "Bash".to_string(),
        description: "A Bash for excuting command".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command to execute"},
                "timeout": {"type": "number", "description": "Optional timeout in milliseconds", "max": 600000}
            },
            "required": ["command"]
        }),
    };

    let content = format!(
        r#"
All the commands that you need have already been executed.

{}
"#,
        combo
            .instructions
            .iter()
            .map(|instruction| match instruction {
                Instruction::Text(text) => text.clone(),
                Instruction::Command { command, output } => {
                    format!("Command: {}\nOutput: {}", command, output)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    )
    .trim()
    .to_string();

    let mut messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Text(content),
    }];

    println!("Send messages: {:?}", messages);

    // Send message with tool
    let response = MessagesBuilder::builder(model, messages.clone(), 1024)
        .credentials(credentials)
        .tools(vec![bash_tool])
        .tool_choice(ToolChoice::Any)
        .create()
        .await
        .unwrap();

    // Process response
    for content in response.content {
        match content {
            ResponseContentBlock::Text { text } => {
                println!("Assistant: {}", text.trim());
                messages.push(Message {
                    role: MessageRole::Assistant,
                    content: MessageContent::Text(text),
                });
            }
            ResponseContentBlock::ToolUse { name, input, .. } => {
                println!("Assistant decided to use the tool: {}: {}", name, input);
                Executor::default().execute(&name, input).await;
            }
            ResponseContentBlock::Thinking {
                signature,
                thinking,
            } => {
                println!("Assistant {} is thinking: {}", signature, thinking);
            }
            ResponseContentBlock::RedactedThinking { data } => {
                println!("Assistant is thinking: {}", data);
            }
        }
    }
}
