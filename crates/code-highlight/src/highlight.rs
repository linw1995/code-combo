use snafu::prelude::*;

use crate::{Lang, error::Result, lang::new_config};
use tree_sitter_highlight::HighlightEvent;
use tree_sitter_highlight::{Highlight, Highlighter};

#[derive(Debug, Eq, PartialEq)]
pub enum Event<'a> {
    Start(&'a str),
    Source(&'a str),
    End,
}

pub fn highlight<'a>(lang: &'_ Lang, names: &'a [&str], source: &'a str) -> Result<Vec<Event<'a>>> {
    let config = new_config(lang, names)?;

    let mut highlighter = Highlighter::new();
    let highlights = highlighter
        .highlight(&config, source.as_bytes(), None, |_| None)
        .unwrap();

    let mut events = vec![];
    for event in highlights {
        match event.whatever_context("failed to highlight code")? {
            HighlightEvent::Source { start, end } => {
                events.push(Event::Source(&source[start..end]));
            }
            HighlightEvent::HighlightStart(Highlight(idx)) => {
                events.push(Event::Start(names[idx]));
            }
            HighlightEvent::HighlightEnd => {
                events.push(Event::End);
            }
        }
    }
    Ok(events)
}
