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
pub mod runner;
mod session;
mod session_env;
mod starter;
mod types;

pub use discovery::*;
pub use runner::{RUN_COMBO_TOOL_NAME, run_combo};
pub use session::{
    ClientMessage, ComboRunEvent, ComboRunMessage, ComboRunPayload, ComboRunResult,
    ComboRunSession, ComboStreamKind, ControlAction, MetadataPayload, MetadataResponse,
    PromptPayload, PromptSchema, RecordChunkPayload, RecordControl, RecordEndPayload,
    RecordSession, RecordStartPayload, ReplyPayload, ReplyValidation, ServerConnection,
    ServerMessage, SessionClientError, SessionServerError, SessionSocketClient,
    SessionSocketServer, ThinkingConfig,
};
pub use session_env::{SESSION_SOCKET_ENV, SessionEnv, SessionEnvBuilder, SessionEnvError};
pub use starter::PromptResponseSender;
pub use starter::{Starter, StarterCommand, StarterError, StarterEvent, StarterExecution};
pub use types::{
    ComboEvent, ComboEventStreamKind, ComboInfo, RunComboContext, RunComboInput, RunComboOutput,
};
