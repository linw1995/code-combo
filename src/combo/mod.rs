use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComboMetadata {
    name: String,
    description: String,
    #[serde(flatten)]
    mode: Mode,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Mode {
    BashXtrace {
        command_prefix: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize, Deserialize)]
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

pub use parser::parse;
