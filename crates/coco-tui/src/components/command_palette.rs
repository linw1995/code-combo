use std::{collections::HashSet, iter};

use coco_macro::ComponentExt;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin},
    prelude::Rect,
    symbols::border,
    text::Text,
    widgets::{Block, Borders, Clear, Widget},
};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tracing::warn;

use crate::{
    actions::CommandPaletteAction,
    components::{Component, Persistable, shortcuts_desc},
    error::Result,
    global::{self, State},
    session::{self, Session},
    theme,
    widgets::Paragraph,
};

const COMMAND_NEW_SESSION: &str = "New Session";
const COMMAND_TRANSCRIPT: &str = "Transcript";
const COMMAND_SWITCH_SESSION: &str = "Switch Session";
const COMMAND_SWITCH_THEME: &str = "Switch Theme";
const COMMAND_SWITCH_MODEL: &str = "Switch Model";
const COMMAND_SHELL: &str = "Shell";

const BREADCRUMB_ROOT: &str = "Command Palette";
const BREADCRUMB_SESSIONS: &str = "Sessions";
const BREADCRUMB_THEMES: &str = "Themes";
const BREADCRUMB_MODELS: &str = "Models";

const SESSION_SWITCH_LIMIT: usize = 20;

#[derive(Clone, Serialize, Deserialize)]
pub struct Command {
    pub name: String,
    pub shortcut: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CommandPaletteMode {
    Main,
    SwitchSession,
    SwitchTheme,
    SwitchModel,
}

#[derive(Clone, Serialize, Deserialize)]
struct CommandListState {
    commands: Vec<Command>,
    focus: Option<usize>,
}

struct CommandList {
    state: State<CommandListState>,
}

impl CommandList {
    fn new(commands: Vec<Command>) -> Self {
        let focus = if commands.is_empty() { None } else { Some(0) };
        Self {
            state: State::new(CommandListState { commands, focus }),
        }
    }

    fn from_state(state: CommandListState) -> Self {
        Self {
            state: State::new(state),
        }
    }

    fn set_commands(&mut self, commands: Vec<Command>) {
        let focus = if commands.is_empty() { None } else { Some(0) };
        let mut state = self.state.write();
        state.commands = commands;
        state.focus = focus;
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

    fn selected_index(&self) -> Option<usize> {
        self.state.focus
    }

    fn selected_command(&self) -> Option<&Command> {
        self.state
            .focus
            .and_then(|idx| self.state.commands.get(idx))
    }

    fn draw(&self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

        let height = self.state.commands.len();
        let constraints = iter::repeat_n(Constraint::Length(1), height);
        let chunks = Layout::vertical(constraints).split(area);

        let theme = global::theme();
        for (idx, command) in self.state.commands.iter().enumerate() {
            let area = chunks[idx];

            let is_focus = Some(idx) == self.state.focus;
            let row_style = if is_focus {
                theme.ui.command_palette_item_bg_focus
            } else {
                theme.ui.command_palette_item_bg
            };
            let mut block = Block::new().borders(Borders::LEFT).style(row_style);
            block = if is_focus {
                block.border_set(border::THICK)
            } else {
                block
                    .border_set(border::PLAIN)
                    .border_style(theme.ui.shortcut_desc)
            };
            frame.render_widget(&block, area);
            let area = block.inner(area);

            let area = if let Some(shortcut) = &command.shortcut {
                let [left, right] =
                    Layout::horizontal([Percentage(50), Percentage(50)]).areas(area);

                frame.render_widget(
                    Text::from(shortcut.to_owned())
                        .style(theme.ui.shortcut)
                        .right_aligned(),
                    right,
                );

                left
            } else {
                area
            };

            let mut name = Text::from(command.name.clone()).left_aligned();
            if !is_focus {
                name = name.style(theme.ui.shortcut_desc)
            }
            frame.render_widget(Paragraph::new(name), area);
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct BreadcrumbState {
    items: Vec<String>,
}

struct Breadcrumb {
    state: State<BreadcrumbState>,
}

impl Breadcrumb {
    fn new(items: Vec<String>) -> Self {
        Self {
            state: State::new(BreadcrumbState { items }),
        }
    }

    fn set_items(&mut self, items: Vec<String>) {
        self.state.write().items = items;
    }

    fn draw(&self, frame: &mut Frame, area: Rect) -> Result<()> {
        let theme = global::theme();
        let items = &self.state.items;
        let label = if items.is_empty() {
            String::new()
        } else {
            items.join(" / ")
        };
        frame.render_widget(
            Paragraph::new(Text::from(label).style(theme.ui.shortcut_desc)),
            area,
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct SessionSwitchEntry {
    label: String,
    metadata: session::PersistentSessionMetadata,
}

#[derive(Clone, Debug)]
struct ModelEntry {
    label: String,
    model: Option<String>,
}

/// Command Palette is a popup floating window that allows users to quickly execute commands.
///
/// This component provides a floating interface that can be triggered to access various
/// application commands and actions. It typically appears as an overlay on top of other
/// UI elements and disappears after a command is executed or when dismissed.
#[derive(ComponentExt)]
#[component(type_id = "command_palette")]
pub struct CommandPalette {
    command_list: CommandList,
    breadcrumb: Breadcrumb,
    mode: CommandPaletteMode,
    session_switch_entries: Vec<SessionSwitchEntry>,
    model_entries: Vec<ModelEntry>,
    current_session_created_at: Option<OffsetDateTime>,
    current_model_override: Option<String>,
    last_model_override: Option<String>,
    auto_model_label: Option<String>,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        let command_list = CommandList::new(Self::main_commands());
        let breadcrumb = Breadcrumb::new(vec![BREADCRUMB_ROOT.to_string()]);
        Self {
            command_list,
            breadcrumb,
            mode: CommandPaletteMode::Main,
            session_switch_entries: Vec::new(),
            model_entries: Vec::new(),
            current_session_created_at: None,
            current_model_override: None,
            last_model_override: None,
            auto_model_label: None,
        }
    }

    pub fn open(
        &mut self,
        current_session_created_at: OffsetDateTime,
        current_model_override: Option<String>,
        last_model_override: Option<String>,
        auto_model_label: Option<String>,
    ) {
        self.current_session_created_at = Some(current_session_created_at);
        self.current_model_override = current_model_override;
        self.last_model_override = last_model_override;
        self.auto_model_label = auto_model_label;
        self.open_main();
    }

    pub fn on_escape(&mut self) -> bool {
        match self.mode {
            CommandPaletteMode::SwitchSession => {
                self.open_main();
                true
            }
            CommandPaletteMode::SwitchTheme => {
                self.open_main();
                true
            }
            CommandPaletteMode::SwitchModel => {
                self.open_main();
                true
            }
            CommandPaletteMode::Main => false,
        }
    }

    fn main_commands() -> Vec<Command> {
        vec![
            Command {
                name: COMMAND_NEW_SESSION.to_string(),
                shortcut: Some("<C-n>".to_string()),
            },
            Command {
                name: COMMAND_TRANSCRIPT.to_string(),
                shortcut: Some("<C-t>".to_string()),
            },
            Command {
                name: COMMAND_SWITCH_SESSION.to_string(),
                shortcut: Some("<C-s>".to_string()),
            },
            Command {
                name: COMMAND_SWITCH_THEME.to_string(),
                shortcut: Some("<C-l>".to_string()),
            },
            Command {
                name: COMMAND_SWITCH_MODEL.to_string(),
                shortcut: Some("<C-o>".to_string()),
            },
            Command {
                name: COMMAND_SHELL.to_string(),
                shortcut: Some("<C-x>".to_string()),
            },
        ]
    }

    fn open_main(&mut self) {
        self.mode = CommandPaletteMode::Main;
        self.session_switch_entries.clear();
        self.model_entries.clear();
        self.command_list.set_commands(Self::main_commands());
        self.breadcrumb.set_items(vec![BREADCRUMB_ROOT.to_string()]);
    }

    fn open_session_switcher(&mut self) {
        let entries = self.build_session_switch_entries(self.current_session_created_at);
        self.mode = CommandPaletteMode::SwitchSession;
        self.breadcrumb.set_items(vec![
            BREADCRUMB_ROOT.to_string(),
            BREADCRUMB_SESSIONS.to_string(),
        ]);

        if entries.is_empty() {
            self.session_switch_entries.clear();
            self.command_list.set_commands(vec![Command {
                name: "No sessions found".to_string(),
                shortcut: None,
            }]);
            return;
        }

        let commands = entries
            .iter()
            .map(|entry| Command {
                name: entry.label.clone(),
                shortcut: None,
            })
            .collect();

        self.session_switch_entries = entries;
        self.command_list.set_commands(commands);
    }

    fn open_theme_switcher(&mut self) {
        self.mode = CommandPaletteMode::SwitchTheme;
        self.breadcrumb.set_items(vec![
            BREADCRUMB_ROOT.to_string(),
            BREADCRUMB_THEMES.to_string(),
        ]);

        let mut themes: Vec<String> = theme::BUILTIN_THEME_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let current_theme = global::config_sync().ui.theme;
        themes.sort_by_key(|name| name != &current_theme);
        let commands = themes
            .into_iter()
            .map(|name| Command {
                name,
                shortcut: None,
            })
            .collect();

        self.command_list.set_commands(commands);
    }

    fn open_model_switcher(&mut self) {
        self.mode = CommandPaletteMode::SwitchModel;
        self.breadcrumb.set_items(vec![
            BREADCRUMB_ROOT.to_string(),
            BREADCRUMB_MODELS.to_string(),
        ]);

        let entries = Self::build_model_entries(
            self.current_model_override.as_deref(),
            self.last_model_override.as_deref(),
            self.auto_model_label.as_deref(),
        );

        let commands = entries
            .iter()
            .map(|entry| Command {
                name: entry.label.clone(),
                shortcut: None,
            })
            .collect();

        self.model_entries = entries;
        self.command_list.set_commands(commands);
    }

    fn build_session_switch_entries(
        &self,
        current_session_created_at: Option<OffsetDateTime>,
    ) -> Vec<SessionSwitchEntry> {
        let mut sessions = self.list_session_metadata();
        if let Some(current_session_created_at) = current_session_created_at {
            sessions.retain(|session| session.created_at != current_session_created_at);
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sessions.truncate(SESSION_SWITCH_LIMIT);

        sessions
            .into_iter()
            .map(|metadata| {
                let label = Self::format_session_label(&metadata);
                SessionSwitchEntry { label, metadata }
            })
            .collect()
    }

    fn build_model_entries(
        current_model_override: Option<&str>,
        last_model_override: Option<&str>,
        auto_model_label: Option<&str>,
    ) -> Vec<ModelEntry> {
        let config = global::config_sync();
        let mut entries = Vec::new();
        let mut seen = HashSet::new();

        for provider in &config.providers {
            let models = match provider.models.as_ref() {
                Some(models) if !models.is_empty() => models.as_slice(),
                _ => {
                    if seen.insert(provider.name.clone()) {
                        entries.push(ModelEntry {
                            label: format!("{} (provider)", provider.name),
                            model: Some(provider.name.clone()),
                        });
                    }
                    continue;
                }
            };

            for model in models {
                if seen.insert(model.clone()) {
                    entries.push(ModelEntry {
                        label: model.clone(),
                        model: Some(model.clone()),
                    });
                }
            }
        }

        let mut last_found = false;
        for entry in entries.iter_mut() {
            let Some(model) = entry.model.as_deref() else {
                continue;
            };
            let is_current = Some(model) == current_model_override;
            let is_last = Some(model) == last_model_override;
            if is_last {
                last_found = true;
            }
            let suffix = match (is_current, is_last) {
                (true, true) => " (current, last)",
                (true, false) => " (current)",
                (false, true) => " (last)",
                (false, false) => "",
            };
            if !suffix.is_empty() {
                entry.label = format!("{}{}", entry.label, suffix);
            }
        }

        if let Some(current_model) = current_model_override
            && let Some(idx) = entries
                .iter()
                .position(|entry| entry.model.as_deref() == Some(current_model))
        {
            let entry = entries.remove(idx);
            entries.insert(0, entry);
        }

        entries.insert(
            0,
            ModelEntry {
                label: match auto_model_label {
                    Some(model) => format!("Auto (default: {model})"),
                    None => "Auto (default)".to_string(),
                },
                model: None,
            },
        );

        if let Some(last_model) = last_model_override
            && !last_found
        {
            let is_current = Some(last_model) == current_model_override;
            let suffix = if is_current {
                " (current, last)"
            } else {
                " (last)"
            };
            entries.insert(
                1,
                ModelEntry {
                    label: format!("{last_model}{suffix}"),
                    model: Some(last_model.to_string()),
                },
            );
        }

        entries
    }

    fn list_session_metadata(&self) -> Vec<session::PersistentSessionMetadata> {
        let session_dir = std::path::Path::new(".coco/sessions").to_path_buf();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(session::list_session(&session_dir))
        });

        match result {
            Ok(sessions) => sessions,
            Err(err) => {
                warn!(?err, "failed to list sessions");
                Vec::new()
            }
        }
    }

    fn format_session_label(metadata: &session::PersistentSessionMetadata) -> String {
        let updated_at = metadata
            .updated_at
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string());
        format!("{} [{}]", metadata.name, updated_at)
    }

    fn on_enter(&mut self) -> Option<CommandPaletteAction> {
        match self.mode {
            CommandPaletteMode::Main => {
                let name = self
                    .command_list
                    .selected_command()
                    .map(|command| command.name.clone());
                match name.as_deref() {
                    Some(COMMAND_SWITCH_SESSION) => {
                        self.open_session_switcher();
                        None
                    }
                    Some(COMMAND_SWITCH_THEME) => {
                        self.open_theme_switcher();
                        None
                    }
                    Some(COMMAND_SWITCH_MODEL) => {
                        self.open_model_switcher();
                        None
                    }
                    Some(COMMAND_NEW_SESSION) => Some(CommandPaletteAction::NewSession),
                    Some(COMMAND_TRANSCRIPT) => Some(CommandPaletteAction::Transcript),
                    Some(COMMAND_SHELL) => Some(CommandPaletteAction::Shell),
                    Some(unknown) => {
                        warn!(?unknown, "unknown command");
                        None
                    }
                    None => None,
                }
            }
            CommandPaletteMode::SwitchSession => {
                let metadata = self
                    .command_list
                    .selected_index()
                    .and_then(|idx| self.session_switch_entries.get(idx))
                    .map(|entry| entry.metadata.clone());

                if let Some(metadata) = metadata {
                    self.open_main();
                    Some(CommandPaletteAction::RestoreSession(metadata))
                } else {
                    None
                }
            }
            CommandPaletteMode::SwitchTheme => {
                let name = self
                    .command_list
                    .selected_command()
                    .map(|command| command.name.clone());

                if let Some(name) = name
                    && theme::BUILTIN_THEME_NAMES.contains(&name.as_str())
                {
                    self.open_main();
                    Some(CommandPaletteAction::SwitchTheme(name))
                } else {
                    None
                }
            }
            CommandPaletteMode::SwitchModel => {
                let entry = self
                    .command_list
                    .selected_index()
                    .and_then(|idx| self.model_entries.get(idx))
                    .cloned();

                entry.map(|entry| {
                    self.open_main();
                    CommandPaletteAction::SwitchModel(entry.model)
                })
            }
        }
    }
}

impl Persistable for CommandPalette {
    fn save(&self) -> Session {
        session::save(&self.command_list.state)
    }

    fn load(state: Session) -> Result<Self> {
        let state: CommandListState = session::load(state)?;
        let command_list = CommandList::from_state(state);
        let breadcrumb = Breadcrumb::new(vec![BREADCRUMB_ROOT.to_string()]);
        Ok(Self {
            command_list,
            breadcrumb,
            mode: CommandPaletteMode::Main,
            session_switch_entries: Vec::new(),
            model_entries: Vec::new(),
            current_session_created_at: None,
            current_model_override: None,
            last_model_override: None,
            auto_model_label: None,
        })
    }
}

impl Component for CommandPalette {
    fn handle_key_event(&mut self, key: &KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as KM;

        let action = match (key.modifiers, key.code) {
            (KM::CONTROL, Char('n' | 'N')) => Some(CommandPaletteAction::NewSession),
            (KM::CONTROL, Char('t' | 'T')) => Some(CommandPaletteAction::Transcript),
            (KM::CONTROL, Char('s' | 'S')) => {
                if self.mode == CommandPaletteMode::Main {
                    self.open_session_switcher();
                }
                None
            }
            (KM::CONTROL, Char('l' | 'L')) => {
                if self.mode == CommandPaletteMode::Main {
                    self.open_theme_switcher();
                }
                None
            }
            (KM::CONTROL, Char('o' | 'O')) => {
                if self.mode == CommandPaletteMode::Main {
                    self.open_model_switcher();
                }
                None
            }
            (KM::CONTROL, Char('x' | 'X')) => {
                if self.mode == CommandPaletteMode::Main {
                    Some(CommandPaletteAction::Shell)
                } else {
                    None
                }
            }
            (KM::NONE, Char('k')) => {
                self.command_list.select_prev();
                None
            }
            (KM::NONE, Char('j')) => {
                self.command_list.select_next();
                None
            }
            (KM::NONE, Enter) => self.on_enter(),
            (KM::NONE, Esc) => unreachable!("Esc key should be handled by the parent component"),
            _ => None,
        };

        if let Some(action) = action {
            global::action_tx().send(action.into()).unwrap();
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::*;

        let margin_background = Margin {
            horizontal: 3,
            vertical: 1,
        };
        let margin_content = Margin {
            horizontal: 3,
            vertical: 1,
        };
        let (width, height) = (
            120 + (margin_background.horizontal + margin_content.horizontal) * 2,
            20 + (margin_background.vertical + margin_content.vertical) * 2,
        );

        // Get the center area of viewport
        let area_popup = {
            let [_, area_h_center, _] =
                Layout::horizontal([Fill(1), Max(width), Fill(1)]).areas(area);
            let [_, area_floating, _] =
                Layout::vertical([Fill(1), Max(height), Fill(1)]).areas(area_h_center);
            area_floating
        };

        // Clear the center area
        Clear.render(area_popup, frame.buffer_mut());
        let theme = global::theme();
        let popup_bg = Block::new().style(theme.ui.chat_bg);
        frame.render_widget(&popup_bg, area_popup);
        let area_background = area_popup.inner(margin_background);

        // Render backgroud color
        let block = Block::new().style(theme.ui.command_palette_bg);
        frame.render_widget(&block, area_background);

        let area_content = area_background.inner(margin_content);
        let block = Block::new().borders(Borders::BOTTOM);
        let block = block
            .title_bottom("")
            .title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")]))
            .title_bottom(shortcuts_desc(&[("Confirm", "CR")]))
            .title_bottom(shortcuts_desc(&[("Cancel", "Esc")]));

        frame.render_widget(&block, area_content);
        let area_content = block.inner(area_content);
        let [_, area_breadcrumb, _, area_list, _] =
            Layout::vertical([Length(1), Length(1), Length(1), Min(0), Length(1)])
                .areas(area_content);

        self.breadcrumb.draw(frame, area_breadcrumb)?;
        self.command_list.draw(frame, area_list)
    }
}
