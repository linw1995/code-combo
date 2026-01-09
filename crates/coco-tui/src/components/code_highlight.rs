use std::ops::Range;

use coco_highlight::{Event, Lang, highlight};
use coco_macro::{ComponentExt, ContentComponentExt};
use ratatui::{
    Frame,
    prelude::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Wrap,
};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use tracing::trace;

use super::{CacheInvalidation, Component, Content, ContentComponent};
use crate::{
    components::Persistable,
    error::Result,
    global,
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Debug, Serialize, Deserialize)]
struct State {
    source: String,
    lang: Lang,
    #[serde(default)]
    overlays: Vec<HighlightRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HighlightRange {
    start: usize,
    end: usize,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "code_highlight")]
pub struct CodeHighlight<'a> {
    state: State,
    widget: Paragraph<'a>,
    theme_dirty: bool,
}

impl<'a> CodeHighlight<'a> {
    pub fn try_new(source: &str, lang: Lang) -> Result<Self> {
        Self::try_new_with_ranges(source, lang, Vec::new())
    }

    pub fn try_new_with_ranges(
        source: &str,
        lang: Lang,
        ranges: Vec<Range<usize>>,
    ) -> Result<Self> {
        let overlays = normalize_ranges(ranges, source.len());
        let widget = Self::build_widget(source, lang, &overlays)?;
        Ok(Self {
            state: State {
                source: source.to_string(),
                lang,
                overlays,
            },
            widget,
            theme_dirty: false,
        })
    }

    fn build_widget(
        source: &str,
        lang: Lang,
        overlays: &[HighlightRange],
    ) -> Result<Paragraph<'static>> {
        use Event::*;

        let theme = global::theme();
        let overlay_style = theme.ui.status_warning.add_modifier(Modifier::UNDERLINED);
        let names = theme
            .tree_sitter
            .keys()
            .map(|x| x.as_str())
            .collect::<Vec<_>>();
        trace!(?names, "highlighting with color scheme");
        let events = highlight(&lang, &names, source).whatever_context("failed to highlight")?;

        let mut line = vec![];
        let mut lines = vec![];
        let mut styles: Vec<Style> = vec![];
        let default_style = Style::default();
        let mut offset = 0;
        let mut overlay_index = 0;
        for event in events {
            match event {
                Start(kind) => styles.push(
                    theme
                        .tree_sitter
                        .get(kind)
                        .cloned()
                        .unwrap_or(default_style),
                ),
                Source(src) => {
                    let style = styles.last().cloned().unwrap_or(default_style);
                    if src.contains("\n") {
                        let mut start = 0;
                        for (idx, ch) in src.char_indices() {
                            if ch != '\n' {
                                continue;
                            }
                            let part = &src[start..idx];
                            push_spans_with_overlays(
                                part,
                                offset,
                                style,
                                overlays,
                                overlay_style,
                                &mut overlay_index,
                                &mut line,
                            );
                            offset += part.len();
                            lines.push(line);
                            line = vec![];
                            offset += ch.len_utf8();
                            start = idx + ch.len_utf8();
                        }
                        let part = &src[start..];
                        push_spans_with_overlays(
                            part,
                            offset,
                            style,
                            overlays,
                            overlay_style,
                            &mut overlay_index,
                            &mut line,
                        );
                        offset += part.len();
                    } else {
                        push_spans_with_overlays(
                            src,
                            offset,
                            style,
                            overlays,
                            overlay_style,
                            &mut overlay_index,
                            &mut line,
                        );
                        offset += src.len();
                    }
                }
                End => {
                    styles.pop();
                }
            }
        }
        lines.push(line);
        let widget = Paragraph::new_wrap(
            Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>()),
            Wrap { trim: false },
        );
        Ok(widget)
    }
}

impl Persistable for CodeHighlight<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: State = session::load(session)?;
        let widget = Self::build_widget(&state.source, state.lang, &state.overlays)?;
        Ok(Self {
            state,
            widget,
            theme_dirty: false,
        })
    }
}

impl Component for CodeHighlight<'static> {
    fn on_cache_invalidation(&mut self, reason: CacheInvalidation) {
        if matches!(reason, CacheInvalidation::Theme) {
            self.theme_dirty = true;
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if self.theme_dirty {
            self.widget =
                Self::build_widget(&self.state.source, self.state.lang, &self.state.overlays)?;
            self.theme_dirty = false;
        }
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl<'a> Content for CodeHighlight<'a> {
    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }
}

impl ContentComponent for CodeHighlight<'static> {}

fn normalize_ranges(ranges: Vec<Range<usize>>, source_len: usize) -> Vec<HighlightRange> {
    let mut normalized = ranges
        .into_iter()
        .filter_map(|range| {
            let start = range.start.min(source_len);
            let end = range.end.min(source_len);
            if start < end { Some(start..end) } else { None }
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|range| range.start);

    let mut merged: Vec<HighlightRange> = Vec::new();
    for range in normalized {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
            continue;
        }
        merged.push(HighlightRange {
            start: range.start,
            end: range.end,
        });
    }
    merged
}

fn push_spans_with_overlays(
    text: &str,
    offset: usize,
    base_style: Style,
    overlays: &[HighlightRange],
    overlay_style: Style,
    overlay_index: &mut usize,
    line: &mut Vec<Span<'static>>,
) {
    if text.is_empty() {
        return;
    }
    let mut cursor = 0;
    while cursor < text.len() {
        while *overlay_index < overlays.len() && overlays[*overlay_index].end <= offset + cursor {
            *overlay_index += 1;
        }
        if *overlay_index >= overlays.len() {
            line.push(Span::styled(text[cursor..].to_owned(), base_style));
            break;
        }
        let overlay = &overlays[*overlay_index];
        if overlay.start >= offset + text.len() {
            line.push(Span::styled(text[cursor..].to_owned(), base_style));
            break;
        }
        let overlay_start = overlay.start.saturating_sub(offset);
        if overlay_start > cursor {
            line.push(Span::styled(
                text[cursor..overlay_start].to_owned(),
                base_style,
            ));
            cursor = overlay_start;
        }
        let overlay_end = overlay.end.saturating_sub(offset).min(text.len());
        let patched = base_style.patch(overlay_style);
        line.push(Span::styled(text[cursor..overlay_end].to_owned(), patched));
        cursor = overlay_end;
        if overlay.end <= offset + cursor {
            *overlay_index += 1;
        }
    }
}
