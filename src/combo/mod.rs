use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComboMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(flatten)]
    pub mode: ComboMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ComboMode {
    BashXtrace {
        command_prefix: String,
    },
    #[serde(other)]
    Unknown,
}

impl Display for ComboMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComboMode::BashXtrace { command_prefix } => f.write_fmt(format_args!(
                "bash_xtrace (command_prefix: {})",
                command_prefix
            )),
            ComboMode::Unknown => f.write_str("unknown"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Instruction {
    Text(String),
    Command { command: String, output: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Combo {
    pub metadata: ComboMetadata,
    pub instructions: Vec<Instruction>,
}

impl Combo {
    pub fn to_markdown(&self) -> String {
        self.instructions
            .iter()
            .map(|instruction| match instruction {
                Instruction::Text(text) => text.clone(),
                Instruction::Command { command, output } => [
                    "I executed this command:\n",
                    "```",
                    command.as_str(),
                    "```\n",
                    "And it outputs:\n",
                    "```",
                    output.as_str(),
                    "```\n",
                ]
                .join("\n"),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

mod session;
mod session_env;
mod starter;

pub use session::{
    ClientMessage, ControlAction, MetadataPayload, MetadataResponse, PromptPayload,
    RecordChunkPayload, RecordControl, RecordEndPayload, RecordSession, RecordStartPayload,
    ServerMessage, SessionClientError, SessionServerError, SessionSocketClient,
    SessionSocketServer,
};
pub use session_env::{SessionEnv, SessionEnvBuilder, SessionEnvError};
pub use starter::{
    Starter, StarterCommand, StarterError, StarterEvent, StarterExecution, discover_combo_starters,
};
