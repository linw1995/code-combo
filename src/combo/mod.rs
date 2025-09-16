use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComboMetadata {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(flatten)]
    mode: ComboMode,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ComboMode {
    BashXtrace {
        command_prefix: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum Instruction {
    Text(String),
    Command { command: String, output: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Combo {
    pub metadata: ComboMetadata,
    pub instructions: Vec<Instruction>,
}

mod parser;
mod starter;

pub use parser::parse;
pub use starter::execute_starter;
