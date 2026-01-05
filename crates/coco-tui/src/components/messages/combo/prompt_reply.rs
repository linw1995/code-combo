use coco_macro::{ComponentExt, ContentComponentExt};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    text::{Line, Span},
};
use serde::{Deserialize, Serialize};

use code_combo::ToolUse;
use code_highlight::Lang;

use crate::{
    components::{CodeHighlight, Component, Content, ContentComponent, Persistable},
    error::*,
    global,
    session::{self, Session},
    widgets::Paragraph,
};

const PROMPT_REPLY_LABEL: &str = "Reply Params";

#[derive(Serialize, Deserialize)]
struct Inner {
    params_json: String,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "combo_prompt_params")]
pub struct PromptReply {
    state: Inner,
    label: Paragraph<'static>,
    params: CodeHighlight<'static>,
    theme_dirty: bool,
}

impl PromptReply {
    pub fn new(tool_use: &ToolUse) -> Self {
        let params_json = build_params_json(tool_use);
        Self::from_json(params_json)
    }

    fn from_json(params_json: String) -> Self {
        let label = build_prompt_params_label();
        let params =
            CodeHighlight::try_new(&params_json, Lang::Json).expect("failed to new CodeHighlight");
        Self {
            state: Inner { params_json },
            label,
            params,
            theme_dirty: false,
        }
    }
}

fn build_prompt_params_label() -> Paragraph<'static> {
    let theme = global::theme();
    Paragraph::new(Line::from(Span::styled(
        PROMPT_REPLY_LABEL.to_string(),
        theme.ui.tool_label,
    )))
}

fn build_params_json(tool_use: &ToolUse) -> String {
    serde_json::to_string_pretty(&tool_use.input).unwrap_or_else(|_| tool_use.input.to_string())
}

impl Persistable for PromptReply {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: Inner = session::load(session)?;
        Ok(Self::from_json(state.params_json))
    }
}

impl Component for PromptReply {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![&mut self.params as &mut dyn Component].into_iter())
    }

    fn on_cache_invalidation(&mut self, reason: crate::components::CacheInvalidation) {
        if matches!(reason, crate::components::CacheInvalidation::Theme) {
            self.theme_dirty = true;
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if area.height == 0 {
            return Ok(());
        }
        if self.theme_dirty {
            self.label = build_prompt_params_label();
            self.theme_dirty = false;
        }

        let width = area.width.max(1);
        let label_height: u16 = 1;
        let params_height = u16::try_from(self.params.height(width)).unwrap_or(u16::MAX);
        let [area_label, area_params] = Layout::vertical([
            Constraint::Length(label_height),
            Constraint::Length(params_height),
        ])
        .areas(area);
        frame.render_widget(&self.label, area_label);
        self.params.draw(frame, area_params)?;
        Ok(())
    }
}

impl Content for PromptReply {
    fn height(&self, width: u16) -> usize {
        let width = width.max(1);
        1 + self.params.height(width)
    }
}

impl ContentComponent for PromptReply {}
