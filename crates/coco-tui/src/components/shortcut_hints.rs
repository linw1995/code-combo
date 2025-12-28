use coco_macro::ComponentExt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    prelude::*,
    symbols::border,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};

use super::messages::shortcuts_desc;
use crate::{
    components::{Component, Persistable, ShortcutHints},
    error::Result,
    global::{self, State},
    session::{self, Session},
};

#[derive(ComponentExt, Default)]
#[component(type_id = "shortcut_hints")]
pub struct ShortcutHintsPanel {
    state: State<Inner>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct Inner {}

impl ShortcutHintsPanel {
    pub fn decorate_block_top<'a>(&self, block: Block<'a>, hints: &ShortcutHints) -> Block<'a> {
        self.apply_shortcut_hints_top(block, hints)
    }

    pub fn decorate_block_bottom<'a>(&self, block: Block<'a>, hints: &ShortcutHints) -> Block<'a> {
        self.apply_shortcut_hints_bottom(block, hints)
    }

    pub fn draw_popup(
        &self,
        frame: &mut Frame,
        area: Rect,
        hints: &ShortcutHints,
        open: bool,
    ) -> Result<()> {
        use Constraint::*;

        if !open {
            return Ok(());
        }

        if hints.hidden.is_empty() {
            return Ok(());
        }

        let lines: Vec<Line> = hints
            .hidden
            .iter()
            .map(|group| shortcuts_desc(group))
            .collect();

        let max_width = area.width.saturating_sub(4).max(20);
        let width = max_width.min(80);
        let max_height = area.height.saturating_sub(2).max(1);
        let height = (lines.len() as u16).saturating_add(4).min(max_height);

        let [_, area_h_center, _] = Layout::horizontal([Fill(1), Max(width), Fill(1)]).areas(area);
        let [_, area_popup, _] =
            Layout::vertical([Fill(1), Max(height), Fill(1)]).areas(area_h_center);

        Clear.render(area_popup, frame.buffer_mut());
        let theme = global::theme();
        let block = Block::new()
            .borders(Borders::ALL)
            .border_set(border::THICK)
            .border_style(theme.ui.block_border_active)
            .style(theme.ui.command_palette_bg)
            .title(Line::from(" Shortcuts ").bold());
        frame.render_widget(&block, area_popup);
        let area_popup = block.inner(area_popup);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area_popup);

        Ok(())
    }

    fn apply_shortcut_hints_top<'a>(
        &self,
        mut block: Block<'a>,
        hints: &ShortcutHints,
    ) -> Block<'a> {
        for group in &hints.visible {
            block = block.title_top(shortcuts_desc(group));
        }
        if hints.has_hidden() {
            block = block.title_top(shortcuts_desc(&[("Help", "?")]));
        }
        block
    }

    fn apply_shortcut_hints_bottom<'a>(
        &self,
        mut block: Block<'a>,
        hints: &ShortcutHints,
    ) -> Block<'a> {
        for group in &hints.visible {
            block = block.title_bottom(shortcuts_desc(group));
        }
        if hints.has_hidden() {
            block = block.title_bottom(shortcuts_desc(&[("Help", "?")]));
        }
        block
    }
}

impl Persistable for ShortcutHintsPanel {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        Ok(Self {
            state: State::new(state),
        })
    }
}

impl Component for ShortcutHintsPanel {
    fn draw(&mut self, _frame: &mut Frame, _area: Rect) -> Result<()> {
        Ok(())
    }
}
