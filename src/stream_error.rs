use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    Transport,
    Decode,
}

#[derive(Debug, Clone)]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub message: String,
}

impl StreamError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: StreamErrorKind::Transport,
            message: message.into(),
        }
    }

    pub fn decode(message: impl Into<String>) -> Self {
        Self {
            kind: StreamErrorKind::Decode,
            message: message.into(),
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self.kind, StreamErrorKind::Transport)
    }

    pub fn with_context(self, context: impl fmt::Display) -> Self {
        Self {
            kind: self.kind,
            message: format!("{context}: {}", self.message),
        }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for StreamError {}
