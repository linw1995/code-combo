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
use tokio::sync::oneshot;
use tracing::warn;

use super::{CacheInvalidation, Component, Content, ContentComponent};
use crate::{
    components::Persistable,
    error::Result,
    global,
    session::{self, Session},
    theme::FinalizedTheme,
    widgets::Paragraph,
};

#[derive(Debug, Serialize, Deserialize)]
struct State {
    source: String,
    lang: Lang,
    #[serde(default)]
    overlays: Vec<HighlightOverlay>,
}

#[derive(Debug, Clone)]
struct PendingSnapshot {
    source: String,
    width: u16,
}

const MAX_GUIDES: usize = 2;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverlayLevel {
    Info,
    #[default]
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightOverlay {
    start: usize,
    end: usize,
    #[serde(default)]
    level: OverlayLevel,
    #[serde(default = "default_underline")]
    underline: bool,
    #[serde(default)]
    newline_guide: Option<String>,
}

impl HighlightOverlay {
    pub fn new(range: Range<usize>, level: OverlayLevel) -> Self {
        Self {
            start: range.start,
            end: range.end,
            level,
            underline: true,
            newline_guide: None,
        }
    }

    pub fn with_newline_guide(mut self, guide: impl Into<String>) -> Self {
        self.newline_guide = Some(guide.into());
        self
    }

    pub fn with_underline(mut self, underline: bool) -> Self {
        self.underline = underline;
        self
    }

    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    fn same_style(&self, other: &Self) -> bool {
        self.level == other.level
            && self.underline == other.underline
            && self.newline_guide == other.newline_guide
    }
}

fn default_underline() -> bool {
    true
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "code_highlight")]
pub struct CodeHighlight<'a> {
    state: State,
    widget: Paragraph<'a>,
    theme_dirty: bool,
    source_dirty: bool,
    last_width: Option<u16>,
    base_style: Option<Style>,
    pending_rx: Option<oneshot::Receiver<Result<Paragraph<'static>>>>,
    pending_snapshot: Option<PendingSnapshot>,
}

impl<'a> CodeHighlight<'a> {
    pub fn try_new(source: &str, lang: Lang) -> Result<Self> {
        Self::try_new_with_overlays(source, lang, Vec::new())
    }

    pub fn try_new_with_ranges(
        source: &str,
        lang: Lang,
        ranges: Vec<Range<usize>>,
    ) -> Result<Self> {
        let overlays = ranges
            .into_iter()
            .map(|range| HighlightOverlay::new(range, OverlayLevel::Warning))
            .collect();
        Self::try_new_with_overlays(source, lang, overlays)
    }

    pub fn try_new_with_overlays(
        source: &str,
        lang: Lang,
        overlays: Vec<HighlightOverlay>,
    ) -> Result<Self> {
        Self::try_new_with_overlays_and_style(source, lang, overlays, None)
    }

    pub fn try_new_with_style(source: &str, lang: Lang, base_style: Style) -> Result<Self> {
        Self::try_new_with_overlays_and_style(source, lang, Vec::new(), Some(base_style))
    }

    pub fn try_new_with_overlays_and_style(
        source: &str,
        lang: Lang,
        overlays: Vec<HighlightOverlay>,
        base_style: Option<Style>,
    ) -> Result<Self> {
        let overlays = normalize_overlays(overlays, source.len());
        let widget = Self::build_placeholder_widget(source, base_style);
        Ok(Self {
            state: State {
                source: source.to_string(),
                lang,
                overlays,
            },
            widget,
            theme_dirty: false,
            source_dirty: true,
            last_width: None,
            base_style,
            pending_rx: None,
            pending_snapshot: None,
        })
    }

    pub fn append_source(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.state.source.push_str(chunk);
        self.widget
            .append_text(chunk, self.base_style.unwrap_or_default());
        self.source_dirty = true;
        if let Some(width) = self.last_width
            && self.pending_rx.is_none()
        {
            self.spawn_build(width);
        }
    }

    fn build_placeholder_widget(source: &str, base_style: Option<Style>) -> Paragraph<'static> {
        let base_style = base_style.unwrap_or_default();
        let lines = source
            .split('\n')
            .map(|line| Line::from(Span::styled(line.to_string(), base_style)))
            .collect::<Vec<_>>();
        Paragraph::new_wrap(Text::from(lines), Wrap { trim: false })
    }

    fn build_widget(
        source: &str,
        lang: Lang,
        overlays: &[HighlightOverlay],
        width: u16,
        base_style: Option<Style>,
    ) -> Result<Paragraph<'static>> {
        use Event::*;

        let theme = global::theme();
        let base_style = base_style.unwrap_or_default();
        let show_guides = should_show_guides(source, width);
        let mut guide_budget = if show_guides { MAX_GUIDES } else { 0 };
        let names = theme
            .tree_sitter
            .keys()
            .map(|x| x.as_str())
            .collect::<Vec<_>>();
        let events = highlight(&lang, &names, source).whatever_context("failed to highlight")?;

        let mut line = vec![];
        let mut lines = vec![];
        let mut styles: Vec<Style> = vec![];
        let default_style = Style::default().patch(base_style);
        let mut offset = 0;
        let mut line_start_offset = 0;
        let mut overlay_index = 0;
        for event in events {
            match event {
                Start(kind) => {
                    let style = theme
                        .tree_sitter
                        .get(kind)
                        .cloned()
                        .unwrap_or(default_style);
                    styles.push(style.patch(base_style));
                }
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
                                theme,
                                &mut overlay_index,
                                &mut line,
                            );
                            let line_end_offset = offset + part.len();
                            offset = line_end_offset;
                            lines.push(line);
                            push_line_guides(
                                &mut lines,
                                source,
                                line_start_offset,
                                line_end_offset,
                                overlays,
                                theme,
                                &mut guide_budget,
                            );
                            line = vec![];
                            offset += ch.len_utf8();
                            line_start_offset = offset;
                            start = idx + ch.len_utf8();
                        }
                        let part = &src[start..];
                        push_spans_with_overlays(
                            part,
                            offset,
                            style,
                            overlays,
                            theme,
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
                            theme,
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
        push_line_guides(
            &mut lines,
            source,
            line_start_offset,
            offset,
            overlays,
            theme,
            &mut guide_budget,
        );
        let widget = Paragraph::new_wrap(
            Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>()),
            Wrap { trim: false },
        );
        Ok(widget)
    }

    fn spawn_build(&mut self, width: u16) {
        let source = self.state.source.clone();
        let snapshot_source = source.clone();
        let lang = self.state.lang;
        let overlays = self.state.overlays.clone();
        let base_style = self.base_style;
        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let result = Self::build_widget(&source, lang, &overlays, width, base_style);
            tx.send(result).ok();
            global::signal_dirty();
        });
        self.pending_rx = Some(rx);
        self.pending_snapshot = Some(PendingSnapshot {
            source: snapshot_source,
            width,
        });
    }

    fn poll_pending(&mut self) -> bool {
        let Some(rx) = &mut self.pending_rx else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
        };
        self.pending_rx = None;
        let snapshot = self.pending_snapshot.take();
        match result {
            Ok(widget) => {
                let stale = snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.source != self.state.source);
                if !stale {
                    self.widget = widget;
                    self.source_dirty = false;
                } else {
                    self.source_dirty = true;
                }
                if let Some(snapshot) = snapshot {
                    self.last_width = Some(snapshot.width);
                }
            }
            Err(err) => {
                warn!(?err, "failed to build CodeHighlight widget");
                self.source_dirty = true;
                if let Some(snapshot) = snapshot {
                    self.last_width = Some(snapshot.width);
                }
            }
        }
        self.theme_dirty = false;
        true
    }

    fn maybe_spawn(&mut self, width: u16) {
        let needs_rebuild = self.theme_dirty || self.source_dirty || self.last_width != Some(width);
        if !needs_rebuild {
            return;
        }
        if self.pending_rx.is_some() {
            return;
        }
        self.spawn_build(width);
        self.theme_dirty = false;
    }
}

impl Persistable for CodeHighlight<'static> {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: State = session::load(session)?;
        let widget = Self::build_placeholder_widget(&state.source, None);
        Ok(Self {
            state,
            widget,
            theme_dirty: false,
            source_dirty: true,
            last_width: None,
            base_style: None,
            pending_rx: None,
            pending_snapshot: None,
        })
    }
}

impl Component for CodeHighlight<'static> {
    fn on_cache_invalidation(&mut self, reason: CacheInvalidation) {
        if matches!(reason, CacheInvalidation::Theme) {
            self.theme_dirty = true;
        }
    }

    fn on_tick(&mut self) {
        if self.poll_pending() {
            global::signal_dirty();
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let _ = self.poll_pending();
        self.maybe_spawn(area.width);
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

fn normalize_overlays(overlays: Vec<HighlightOverlay>, source_len: usize) -> Vec<HighlightOverlay> {
    let mut normalized = overlays
        .into_iter()
        .filter_map(|mut overlay| {
            overlay.start = overlay.start.min(source_len);
            overlay.end = overlay.end.min(source_len);
            if overlay.start < overlay.end {
                Some(overlay)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|overlay| overlay.start);

    let mut merged: Vec<HighlightOverlay> = Vec::new();
    for overlay in normalized {
        if let Some(last) = merged.last_mut()
            && overlay.start <= last.end
            && overlay.same_style(last)
        {
            last.end = last.end.max(overlay.end);
            continue;
        }
        merged.push(overlay);
    }
    merged
}

fn overlay_base_style(theme: &FinalizedTheme, overlay: &HighlightOverlay) -> Style {
    match overlay.level {
        OverlayLevel::Info => theme.ui.code_overlay_info,
        OverlayLevel::Warning => theme.ui.code_overlay_warning,
        OverlayLevel::Error => theme.ui.code_overlay_error,
    }
}

fn overlay_highlight_style(theme: &FinalizedTheme, overlay: &HighlightOverlay) -> Style {
    let mut style = overlay_base_style(theme, overlay);
    if overlay.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn overlay_guide_style(theme: &FinalizedTheme, overlay: &HighlightOverlay) -> Style {
    overlay_base_style(theme, overlay).add_modifier(Modifier::DIM)
}

fn push_line_guides(
    lines: &mut Vec<Vec<Span<'static>>>,
    source: &str,
    line_start: usize,
    line_end: usize,
    overlays: &[HighlightOverlay],
    theme: &FinalizedTheme,
    guide_budget: &mut usize,
) {
    if *guide_budget == 0 {
        return;
    }
    let line_text = source.get(line_start..line_end).unwrap_or_default();
    let mut guides = overlays
        .iter()
        .filter(|overlay| {
            overlay.newline_guide.is_some()
                && overlay.start >= line_start
                && overlay.start < line_end
        })
        .map(|overlay| {
            let label = overlay.newline_guide.as_ref().expect("newline guide label");
            let column =
                display_column_for_offset(line_text, overlay.start.saturating_sub(line_start));
            GuideMarker::new(column, overlay.level, overlay.range(), label)
        })
        .collect::<Vec<_>>();
    guides.sort_by_key(|guide| guide.range.start);

    let mut merged: Vec<GuideMarker<'_>> = Vec::new();
    for guide in guides {
        if let Some(last) = merged.last_mut()
            && (last.overlaps(&guide) || last.column == guide.column)
        {
            last.merge(guide);
            continue;
        }
        merged.push(guide);
    }

    for guide in merged {
        if *guide_budget == 0 {
            break;
        }
        let style = overlay_guide_style(theme, &guide.style_overlay());
        let padding = " ".repeat(guide.column);
        lines.push(vec![
            Span::raw(padding),
            Span::styled(format!("┗━ {}", guide.label_text()), style),
        ]);
        *guide_budget -= 1;
    }
}

fn push_spans_with_overlays(
    text: &str,
    offset: usize,
    base_style: Style,
    overlays: &[HighlightOverlay],
    theme: &FinalizedTheme,
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
        let patched = base_style.patch(overlay_highlight_style(theme, overlay));
        line.push(Span::styled(text[cursor..overlay_end].to_owned(), patched));
        cursor = overlay_end;
        if overlay.end <= offset + cursor {
            *overlay_index += 1;
        }
    }
}

fn display_column_for_offset(line: &str, byte_offset: usize) -> usize {
    const TAB_STOP: usize = 4;
    let mut col = 0;
    let mut idx = 0;
    let bytes = line.as_bytes();
    while idx < bytes.len() && idx < byte_offset {
        let ch = line[idx..].chars().next().unwrap_or('\0');
        let len = ch.len_utf8();
        if ch == '\t' {
            let advance = TAB_STOP - (col % TAB_STOP);
            col += advance;
        } else {
            col += 1;
        }
        idx += len;
    }
    col
}

fn should_show_guides(source: &str, width: u16) -> bool {
    if width == 0 {
        return false;
    }
    let max_width = width as usize;
    for line in source.split('\n') {
        if display_column_for_offset(line, line.len()) > max_width {
            return false;
        }
    }
    true
}

struct GuideMarker<'a> {
    column: usize,
    level: OverlayLevel,
    range: Range<usize>,
    labels: Vec<&'a str>,
}

impl<'a> GuideMarker<'a> {
    fn new(column: usize, level: OverlayLevel, range: Range<usize>, label: &'a str) -> Self {
        Self {
            column,
            level,
            range,
            labels: vec![label],
        }
    }

    fn severity(&self) -> usize {
        match self.level {
            OverlayLevel::Info => 0,
            OverlayLevel::Warning => 1,
            OverlayLevel::Error => 2,
        }
    }

    fn merge(&mut self, other: GuideMarker<'a>) {
        if other.severity() > self.severity() {
            self.level = other.level;
        }
        self.range.start = self.range.start.min(other.range.start);
        self.range.end = self.range.end.max(other.range.end);
        if other.column < self.column {
            self.column = other.column;
        }
        for label in other.labels {
            if !self.labels.contains(&label) {
                self.labels.push(label);
            }
        }
    }

    fn label_text(&self) -> String {
        self.labels.join(" / ")
    }

    fn style_overlay(&self) -> HighlightOverlay {
        HighlightOverlay::new(self.range.clone(), self.level).with_underline(false)
    }

    fn overlaps(&self, other: &GuideMarker<'a>) -> bool {
        self.range.start < other.range.end && other.range.start < self.range.end
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, prelude::Buffer};

    use super::*;

    fn buffer_line(buffer: &Buffer, y: u16, width: u16) -> String {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buffer[(x, y)].symbol());
        }
        line
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn code_highlight_adds_newline_guides() {
        let source = "echo one\necho two";
        let overlay = HighlightOverlay::new(0..source.len(), OverlayLevel::Warning)
            .with_newline_guide("line break");
        let mut widget = CodeHighlight::try_new_with_overlays(source, Lang::Bash, vec![overlay])
            .expect("create code highlight");

        let width = 24;
        let mut terminal = Terminal::new(TestBackend::new(width, 3)).unwrap();
        terminal
            .draw(|frame| widget.draw(frame, frame.area()).unwrap())
            .unwrap();

        let buffer = terminal.backend().buffer();
        let guide_line = buffer_line(buffer, 1, width);
        assert!(
            guide_line.contains("┗━ line break"),
            "unexpected guide line: {guide_line:?}"
        );
    }
}
