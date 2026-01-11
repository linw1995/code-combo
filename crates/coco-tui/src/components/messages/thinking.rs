use coco_macro::{ComponentExt, ContentComponentExt};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    prelude::Rect,
    style::{Modifier, Style},
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::warn;

use super::{
    Component, Content, ShortcutHints, fold::FoldState, plain::ExternalMarkdownViewer,
    plain::RawTextViewer,
};
use crate::{
    components::{CacheInvalidation, CodeHighlight, ContentComponent, Persistable},
    error::*,
    global,
    session::{self, Session},
};
use coco_highlight::Lang;
use code_combo::MarkdownRenderEngine;

type WidgetBuild = (
    Box<dyn ContentComponent>,
    Option<oneshot::Receiver<Box<dyn ContentComponent>>>,
);

#[derive(Serialize, Deserialize)]
struct ThinkingState {
    text: String,
    fold_state: FoldState,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "thinking")]
pub struct Thinking {
    state: ThinkingState,
    widget: Box<dyn ContentComponent>,
    rx: Option<oneshot::Receiver<Box<dyn ContentComponent>>>,
}

impl Thinking {
    pub fn new(text: String) -> Self {
        Self::new_with_external(
            ThinkingState {
                text,
                fold_state: FoldState::Expanded,
            },
            true,
        )
    }

    pub fn new_stream(text: String) -> Self {
        Self::new_with_external(
            ThinkingState {
                text,
                fold_state: FoldState::Expanded,
            },
            false,
        )
    }

    pub fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.state.text.push_str(text);
        if let Some(widget) = self
            .widget
            .as_mut_any()
            .downcast_mut::<CodeHighlight<'static>>()
        {
            widget.append_source(text);
            return;
        }
        self.refresh_widget(false);
    }

    pub fn finalize_stream(&mut self) {
        self.refresh_widget(true);
    }

    pub fn collapse(&mut self) {
        self.state.fold_state.collapse();
    }

    pub fn toggle(&mut self) {
        self.state.fold_state = self.state.fold_state.toggle();
    }

    pub fn is_collapsed(&self) -> bool {
        self.state.fold_state.is_collapsed()
    }

    fn refresh_widget(&mut self, allow_external: bool) {
        let (widget, rx) = Self::build_widget(&self.state.text, allow_external);
        self.widget = widget;
        self.rx = rx;
    }

    fn new_with_external(state: ThinkingState, allow_external: bool) -> Self {
        let (widget, rx) = Self::build_widget(&state.text, allow_external);
        Self { state, widget, rx }
    }

    fn build_widget(text: &str, allow_external: bool) -> WidgetBuild {
        let cfg = global::config_sync();
        let base_style = Style::default().add_modifier(Modifier::DIM);

        let rx = if allow_external {
            match cfg.ui.markdown_render_engine {
                MarkdownRenderEngine::ExternalCommand { executable, args } => {
                    let (tx, rx) = oneshot::channel();
                    tokio::task::spawn({
                        let text = text.to_string();
                        async move {
                            match ExternalMarkdownViewer::try_new_with_style(
                                &text,
                                &executable,
                                &args,
                                base_style,
                            )
                            .await
                            {
                                Ok(widget) => {
                                    tx.send(widget.into()).ok();
                                }
                                Err(err) => {
                                    warn!(
                                        ?err,
                                        "failed using an external CLI tool to render Markdown"
                                    );
                                }
                            };
                        }
                    });
                    Some(rx)
                }
                MarkdownRenderEngine::Native => None,
            }
        } else {
            None
        };

        let widget = CodeHighlight::try_new_with_style(text, Lang::Markdown, base_style)
            .map(|x| x.into())
            .unwrap_or_else(|err| {
                warn!(?err, "failed to new CodeHighlight Component");
                RawTextViewer::new_with_style(text.to_string(), base_style).into()
            });
        (widget, rx)
    }
}

impl Persistable for Thinking {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: ThinkingState = session::load(session)?;
        Ok(Self::new_with_external(state, true))
    }
}

impl Component for Thinking {
    fn on_cache_invalidation(&mut self, reason: CacheInvalidation) {
        self.widget.invalidate_cache(reason);
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        if matches!(key.code, KeyCode::Char('r' | 'R' | 'z' | 'Z')) {
            self.toggle();
            global::signal_dirty();
        }
    }

    fn on_tick(&mut self) {
        if let Some(rx) = &mut self.rx {
            let Ok(widget) = rx.try_recv() else {
                return;
            };
            self.widget = widget;
            self.rx = None;
            global::signal_dirty();
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if self.is_collapsed() {
            return Ok(());
        }
        self.widget.draw(frame, area)
    }
}

impl Content for Thinking {
    fn height(&self, width: u16) -> usize {
        if width == 0 {
            return 0;
        }
        if self.is_collapsed() {
            return 0;
        }
        self.widget.height(width)
    }

    fn shortcut_hints(&self) -> ShortcutHints {
        if self.is_collapsed() {
            return ShortcutHints::default();
        }
        let mut hints = ShortcutHints::default();
        hints.push_visible(&[("Fold", "r")]);
        hints
    }
}

impl ContentComponent for Thinking {}
