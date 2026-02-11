//! State types for the Chat component

use crate::global::State;
use code_combo::Message as ChatMessage;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::ComboTranscript;
use super::SubagentTranscript;

const CTRL_C_WINDOW: Duration = Duration::from_secs(2);

/// Current state of the chat session
#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatState {
    #[default]
    Ready,
    #[serde(alias = "Procesing")]
    Processing,
}

impl std::fmt::Display for ChatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready => f.write_str("Ready"),
            Self::Processing => f.write_str("Processing"),
        }
    }
}

/// Focus state for the chat UI
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum Focus {
    #[default]
    Input,
    InputBlur,
    Messages,
    CommandPalette,
    ShortcutHints,
}

/// View mode for the chat interface
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewMode {
    Chat,
    Transcript,
}

/// Scope for transcript viewing
#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptScope {
    Combo { id: String, name: String },
    Subagent { id: String, name: String },
}

/// Exit shortcut type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExitShortcut {
    CtrlC,
    CtrlQ,
}

/// Guard for tracking double-key shortcuts (Ctrl+C, Ctrl+Q)
#[derive(Debug, Default)]
pub struct CancellationGuard {
    last_hit: State<Option<Instant>>,
    last_shortcut: State<Option<ExitShortcut>>,
    cancel_token: Option<CancellationToken>,
}

impl CancellationGuard {
    pub fn try_fire(&mut self, shortcut: ExitShortcut) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_hit.get()
            && now.duration_since(last) <= CTRL_C_WINDOW
            && self.last_shortcut.get() == Some(shortcut)
        {
            // fire
            self.cancel_token();
            self.reset();
            return true;
        }

        *self.last_hit.write() = Some(now);
        *self.last_shortcut.write() = Some(shortcut);

        false
    }

    pub fn confirm(&mut self, shortcut: ExitShortcut) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_hit.get()
            && now.duration_since(last) <= CTRL_C_WINDOW
            && self.last_shortcut.get() == Some(shortcut)
        {
            self.reset();
            return true;
        }

        *self.last_hit.write() = Some(now);
        *self.last_shortcut.write() = Some(shortcut);

        false
    }

    pub fn reset(&mut self) {
        *self.last_hit.write() = None;
        *self.last_shortcut.write() = None;
    }

    pub fn on_trick(&mut self) {
        let Some(last) = self.last_hit.get() else {
            return;
        };
        if Instant::now().duration_since(last) > CTRL_C_WINDOW {
            self.reset();
        }
    }

    pub fn is_armed(&self) -> bool {
        self.last_hit.is_some()
    }

    pub fn last_shortcut(&self) -> Option<ExitShortcut> {
        self.last_shortcut.get()
    }

    pub fn token(&mut self) -> CancellationToken {
        if let Some(token) = &self.cancel_token {
            return token.clone();
        }
        let token = CancellationToken::new();
        self.cancel_token = Some(token.clone());
        token
    }

    pub fn start_token(&mut self) -> CancellationToken {
        self.cancel_token();
        self.reset();
        self.token()
    }

    fn cancel_token(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
    }
}

/// Inner state persisted across sessions
#[derive(Clone, Serialize, Deserialize)]
pub struct Inner {
    pub state: ChatState,
    pub focus: Focus,
    #[serde(default)]
    pub auto_accept_edits: bool,
    #[serde(default)]
    pub thinking_enabled: bool,
    #[serde(default)]
    pub model_override: Option<String>,
    pub pending_chats: Vec<code_combo::Block>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    pub name: String,

    // Placehold for session persistence
    #[serde(default)]
    pub system_prompt: Option<String>,
    pub messages: Vec<code_combo::Message>,
    #[serde(default)]
    pub combo_transcripts: Vec<ComboTranscript>,
    #[serde(default)]
    pub subagent_transcripts: Vec<SubagentTranscript>,
}

impl Default for Inner {
    fn default() -> Self {
        let now = time::OffsetDateTime::now_utc();
        Self {
            system_prompt: None,
            messages: vec![],
            combo_transcripts: Vec::new(),
            subagent_transcripts: Vec::new(),
            state: ChatState::Ready,
            focus: Focus::Input,
            auto_accept_edits: false,
            thinking_enabled: false,
            model_override: None,
            pending_chats: Vec::new(),
            created_at: now,
            updated_at: now,
            name: format!(
                "Session {}",
                now.format(&time::format_description::well_known::Rfc3339)
                    .expect("failed to format current time")
            ),
        }
    }
}
