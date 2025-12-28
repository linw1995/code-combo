use snafu::prelude::*;

use crate::{Lang, error::Result, lang::new_config};
use tree_sitter::Parser;
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

#[derive(Debug, Eq, PartialEq)]
pub enum Event<'a> {
    Start(&'a str),
    Source(&'a str),
    End,
}

#[derive(Debug, Clone, Copy)]
struct ByteRange {
    start: usize,
    end: usize,
}

struct ConfigEntry {
    lang: Lang,
    config: HighlightConfiguration,
}

fn build_configs(lang: Lang, names: &[&str]) -> Result<Vec<ConfigEntry>> {
    let mut configs = Vec::new();
    let add = |lang: Lang, configs: &mut Vec<ConfigEntry>| -> Result<()> {
        if configs.iter().any(|entry| entry.lang == lang) {
            return Ok(());
        }
        let config = new_config(&lang, names)?;
        configs.push(ConfigEntry { lang, config });
        Ok(())
    };

    add(lang, &mut configs)?;
    for injected in lang.injection_candidates() {
        add(*injected, &mut configs)?;
    }

    Ok(configs)
}

fn config_for_lang(lang: Lang, configs: &[ConfigEntry]) -> Option<&HighlightConfiguration> {
    configs
        .iter()
        .find(|entry| entry.lang == lang)
        .map(|entry| &entry.config)
}

fn config_for_injection<'a>(
    language: &str,
    configs: &'a [ConfigEntry],
) -> Option<&'a HighlightConfiguration> {
    let lang = Lang::from_injection_language(language)?;
    configs
        .iter()
        .find(|entry| entry.lang == lang)
        .map(|entry| &entry.config)
}

pub fn highlight<'a>(lang: &'_ Lang, names: &'a [&str], source: &'a str) -> Result<Vec<Event<'a>>> {
    let configs = build_configs(*lang, names)?;
    let primary =
        config_for_lang(*lang, &configs).whatever_context("missing highlight configuration")?;
    let mut highlighter = Highlighter::new();
    let events = highlight_with_config(&mut highlighter, primary, &configs, names, source)?;

    if *lang == Lang::Markdown {
        return highlight_markdown_inline(events, &configs, names, source);
    }

    Ok(events)
}

fn highlight_with_config<'a>(
    highlighter: &mut Highlighter,
    config: &HighlightConfiguration,
    configs: &[ConfigEntry],
    names: &'a [&str],
    source: &'a str,
) -> Result<Vec<Event<'a>>> {
    let highlights = highlighter
        .highlight(config, source.as_bytes(), None, |language| {
            config_for_injection(language, configs)
        })
        .whatever_context("failed to highlight code")?;

    let mut events = Vec::new();
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

fn highlight_markdown_inline<'a>(
    events: Vec<Event<'a>>,
    configs: &[ConfigEntry],
    names: &'a [&str],
    source: &'a str,
) -> Result<Vec<Event<'a>>> {
    let inline_config = match config_for_lang(Lang::MarkdownInline, configs) {
        Some(config) => config,
        None => return Ok(events),
    };
    let ranges = collect_inline_ranges(source)?;
    if ranges.is_empty() {
        return Ok(events);
    }
    apply_inline_highlights(events, configs, names, source, inline_config, &ranges)
}

fn collect_inline_ranges(source: &str) -> Result<Vec<ByteRange>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .whatever_context("failed to load markdown parser")?;
    let tree = parser
        .parse(source.as_bytes(), None)
        .whatever_context("failed to parse markdown")?;

    let mut ranges = Vec::new();
    collect_inline_nodes(tree.root_node(), &mut ranges);
    if ranges.is_empty() {
        return Ok(ranges);
    }
    ranges.sort_by_key(|range| range.start);

    let mut merged: Vec<ByteRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
            continue;
        }
        merged.push(range);
    }

    Ok(merged)
}

fn collect_inline_nodes(node: tree_sitter::Node<'_>, ranges: &mut Vec<ByteRange>) {
    let kind = node.kind();
    if kind == "inline" || kind == "pipe_table_cell" {
        let start = node.start_byte();
        let end = node.end_byte();
        if start < end {
            ranges.push(ByteRange { start, end });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_inline_nodes(child, ranges);
    }
}

fn apply_inline_highlights<'a>(
    events: Vec<Event<'a>>,
    configs: &[ConfigEntry],
    names: &'a [&str],
    source: &'a str,
    inline_config: &HighlightConfiguration,
    ranges: &[ByteRange],
) -> Result<Vec<Event<'a>>> {
    let mut highlighter = Highlighter::new();
    let mut output = Vec::with_capacity(events.len());
    let mut offset = 0usize;
    let mut range_index = 0usize;

    for event in events {
        match event {
            Event::Source(text) => {
                let start = offset;
                let end = start + text.len();

                while range_index < ranges.len() && ranges[range_index].end <= start {
                    range_index += 1;
                }

                let mut cursor = start;
                let mut active_index = range_index;
                while active_index < ranges.len() {
                    let range = ranges[active_index];
                    if range.start >= end {
                        break;
                    }

                    if range.start > cursor {
                        output.push(Event::Source(&source[cursor..range.start]));
                    }

                    let inline_start = range.start.max(cursor);
                    let inline_end = range.end.min(end);
                    if inline_start < inline_end {
                        let inline_source = &source[inline_start..inline_end];
                        let inline_events = highlight_with_config(
                            &mut highlighter,
                            inline_config,
                            configs,
                            names,
                            inline_source,
                        )?;
                        output.extend(inline_events);
                    }

                    cursor = inline_end;
                    if range.end <= end {
                        active_index += 1;
                    } else {
                        break;
                    }
                }

                if cursor < end {
                    output.push(Event::Source(&source[cursor..end]));
                }

                offset = end;
                range_index = active_index;
            }
            other => output.push(other),
        }
    }

    Ok(output)
}
