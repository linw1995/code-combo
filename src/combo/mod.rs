use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComboMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Combo {
    pub metadata: ComboMetadata,
}

mod discovery;
mod session;
mod session_env;
mod starter;

pub use discovery::*;
pub use session::{
    ClientMessage, ControlAction, MetadataPayload, MetadataResponse, PromptPayload, PromptSchema,
    RecordChunkPayload, RecordControl, RecordEndPayload, RecordSession, RecordStartPayload,
    ServerConnection, ServerMessage, SessionClientError, SessionServerError, SessionSocketClient,
    SessionSocketServer,
};
pub use session_env::{SessionEnv, SessionEnvBuilder, SessionEnvError};
pub use starter::PromptResponseSender;
pub use starter::{Starter, StarterCommand, StarterError, StarterEvent, StarterExecution};
