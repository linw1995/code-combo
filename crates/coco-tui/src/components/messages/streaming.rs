use std::collections::VecDeque;

use code_combo::{OutputChunk, StreamKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StreamedLine {
    pub(crate) stream: StreamKind,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StreamedLines {
    #[serde(default)]
    lines: VecDeque<StreamedLine>,
    #[serde(default)]
    max_lines: Option<usize>,
}

impl StreamedLines {
    pub(crate) fn new(max_lines: Option<usize>) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines,
        }
    }

    pub(crate) fn from_chunks<'a, I>(chunks: I, max_lines: Option<usize>) -> Self
    where
        I: IntoIterator<Item = &'a OutputChunk>,
    {
        let mut lines = Self::new(max_lines);
        for chunk in chunks {
            lines.push_chunk(chunk);
        }
        lines
    }

    pub(crate) fn push_chunk(&mut self, chunk: &OutputChunk) -> usize {
        let mut dropped = 0;
        for text in &chunk.lines {
            dropped += self.push_line(chunk.stream, text.clone());
        }
        dropped
    }

    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &StreamedLine> {
        self.lines.iter()
    }

    fn push_line(&mut self, stream: StreamKind, text: String) -> usize {
        let mut dropped = 0;
        if let Some(max_lines) = self.max_lines {
            while self.lines.len() >= max_lines {
                self.lines.pop_front();
                dropped += 1;
            }
        }
        self.lines.push_back(StreamedLine { stream, text });
        dropped
    }
}
