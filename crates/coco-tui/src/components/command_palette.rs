use std::iter;

use coco_macro::ComponentExt;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    symbols::border,
    text::Text,
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use serde::{Deserialize, Serialize};

use crate::{
    components::{Component, Persistable, shortcuts_desc},
    error::Result,
    global::{self, State},
    session::{self, Session},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Command {
    pub name: String,
    pub shortcut: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Inner {
    pub commands: Vec<Command>,
    pub focus: Option<usize>,
}

/// Command Palette is a popup floating window that allows users to quickly execute commands.
///
/// This component provides a floating interface that can be triggered to access various
/// application commands and actions. It typically appears as an overlay on top of other
/// UI elements and disappears after a command is executed or when dismissed.
#[derive(ComponentExt)]
#[component(type_id = "command_palette")]
pub struct CommandPalette {
    state: State<Inner>,
}

impl CommandPalette {
    pub fn new(commands: &[Command]) -> Self {
        let focus = if commands.is_empty() { None } else { Some(0) };
        Self {
            state: State::new(Inner {
                commands: Vec::from(commands),
                focus,
            }),
        }
    }

    fn select_prev(&mut self) -> bool {
        if let Some(idx) = self.state.focus
            && idx > 0
        {
            self.state.write().focus = Some(idx - 1);
            true
        } else {
            false
        }
    }

    fn select_next(&mut self) -> bool {
        if let Some(idx) = self.state.focus
            && idx < self.state.commands.len() - 1
        {
            self.state.write().focus = Some(idx + 1);
            true
        } else {
            false
        }
    }

    pub fn draw_commands(&self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

        let height = self.state.commands.len();
        let constraints = iter::repeat_n(Constraint::Length(1), height);
        let chunks = Layout::vertical(constraints).split(area);

        let theme = global::theme();
        for (idx, command) in self.state.commands.iter().enumerate() {
            let area = chunks[idx];

            // Command focus highlight
            let mut block = Block::new().borders(Borders::LEFT);
            let is_focus = Some(idx) == self.state.focus;
            block = if is_focus {
                block.border_set(border::THICK)
            } else {
                block
                    .border_set(border::PLAIN)
                    .border_style(theme.ui.shortcut_desc)
            };
            frame.render_widget(&block, area);
            let area = block.inner(area);

            // Command shortcut
            let area = if let Some(shortcut) = &command.shortcut {
                let [left, right] =
                    Layout::horizontal([Percentage(50), Percentage(50)]).areas(area);

                frame.render_widget(
                    Paragraph::new(Text::from(shortcut.to_owned()).style(theme.ui.shortcut))
                        .right_aligned(),
                    right,
                );

                left
            } else {
                area
            };

            // Command name
            let mut name = Text::from(command.name.clone()).left_aligned();
            if !is_focus {
                name = name.style(theme.ui.shortcut_desc)
            }
            frame.render_widget(Paragraph::new(name), area);
        }
        Ok(())
    }
}

impl Persistable for CommandPalette {
    fn save(&self) -> Session {
        session::save(&self.state)
    }
    fn load(state: Session) -> Result<Self> {
        let state: Inner = session::load(state)?;
        Ok(Self {
            state: State::new(state),
        })
    }
}

impl Component for CommandPalette {
    fn handle_key_event(&mut self, key: &KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as KM;

        match (key.modifiers, key.code) {
            (KM::NONE, Char('k')) => {
                self.select_prev();
            }
            (KM::NONE, Char('j')) => {
                self.select_next();
            }
            (KM::NONE, Enter) => {
                unimplemented!()
            }
            (KM::NONE, Esc) => {
                unreachable!("Esc key should be handled by the parent component")
            }
            _ => (),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

        let area = {
            let [_, area_h_center, _] =
                Layout::horizontal([Fill(1), Max(120), Fill(1)]).areas(area);
            let [_, area_floating, _] =
                Layout::vertical([Fill(1), Max(20), Fill(1)]).areas(area_h_center);
            area_floating
        };
        // ensure that all cells under the popup are cleared to avoid leaking content
        Clear.render(area, frame.buffer_mut());

        let block = Block::new().borders(Borders::BOTTOM);
        let block = block
            .title_bottom("")
            .title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")]))
            .title_bottom(shortcuts_desc(&[("Confirm", "CR")]))
            .title_bottom(shortcuts_desc(&[("Cancel", "Esc")]));

        frame.render_widget(&block, area);
        self.draw_commands(frame, block.inner(area))
    }
}
