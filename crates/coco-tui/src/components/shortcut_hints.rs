use coco_macro::ComponentExt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    prelude::*,
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
    hints: ShortcutHints,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct Inner {}

impl ShortcutHintsPanel {
    pub fn set_hints(&mut self, hints: ShortcutHints) {
        self.hints = hints;
    }

    pub fn decorate_block_top<'a>(&self, block: Block<'a>, hints: &ShortcutHints) -> Block<'a> {
        self.apply_shortcut_hints_top(block, hints)
    }

    pub fn decorate_block_bottom<'a>(&self, block: Block<'a>, hints: &ShortcutHints) -> Block<'a> {
        self.apply_shortcut_hints_bottom(block, hints)
    }

    fn draw_popup(&self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

        if self.hints.hidden.is_empty() {
            return Ok(());
        }

        let margin_background = Margin {
            horizontal: 3,
            vertical: 1,
        };
        let margin_content = Margin {
            horizontal: 3,
            vertical: 1,
        };
        let lines: Vec<Line> = self
            .hints
            .hidden
            .iter()
            .map(|group| shortcuts_desc(group))
            .collect();

        let width = 120 + (margin_background.horizontal + margin_content.horizontal) * 2;
        let height = (lines.len() as u16)
            .saturating_add(4 + margin_background.vertical * 2 + margin_content.vertical * 2)
            .min(area.height.saturating_sub(2))
            .max(1);

        let [_, area_h_center, _] = Layout::horizontal([Fill(1), Max(width), Fill(1)]).areas(area);
        let [_, area_popup, _] =
            Layout::vertical([Fill(1), Max(height), Fill(1)]).areas(area_h_center);

        Clear.render(area_popup, frame.buffer_mut());
        let theme = global::theme();
        let area_background = area_popup.inner(margin_background);
        let block = Block::new().style(theme.ui.command_palette_bg);
        frame.render_widget(&block, area_background);

        let area_content = area_background.inner(margin_content);
        let block = Block::new().borders(Borders::BOTTOM);
        let block = block.title_bottom(Line::from(" Shortcuts ").bold());
        frame.render_widget(&block, area_content);
        let area_content = block.inner(area_content);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area_content);

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
            hints: ShortcutHints::default(),
        })
    }
}

impl Component for ShortcutHintsPanel {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.draw_popup(frame, area)
    }
}
