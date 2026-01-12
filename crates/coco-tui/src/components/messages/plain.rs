use std::ops::DerefMut;

use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::MarkdownRenderEngine;
use ratatui::{Frame, prelude::Rect};
use tokio::sync::oneshot;
use tracing::{trace, warn};

use super::{Component, Content};
use crate::components::CacheInvalidation;
use crate::{
    components::{CodeHighlight, ContentComponent, Persistable},
    error::*,
    global,
    session::{self, Session},
};

mod external_viewer;
mod raw;
pub(crate) use external_viewer::ExternalMarkdownViewer;
pub(crate) use raw::RawTextViewer;

type WidgetBuild = (
    Box<dyn ContentComponent>,
    Option<oneshot::Receiver<Box<dyn ContentComponent>>>,
);

/// Plain text render widget.
///
/// Support Markdown syntax with multiple approaches:
/// - Use a built-in Markdown parser and renderer. (Tree-Sitter Highlight, Streaming)
/// - Use an external CLI tool to render Markdown
/// - TODO: Save content to a temporary file and open with external viewer
#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "plain")]
pub struct Plain {
    text: String,
    widget: Box<dyn ContentComponent>,
    rx: Option<oneshot::Receiver<Box<dyn ContentComponent>>>,
}

impl Plain {
    pub fn new(text: String) -> Self {
        Self::new_with_external(text, true)
    }

    pub fn new_stream(text: String) -> Self {
        Self::new_with_external(text, false)
    }

    pub fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.text.push_str(text);
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

    fn refresh_widget(&mut self, allow_external: bool) {
        let (widget, rx) = Self::build_widget(&self.text, allow_external);
        self.widget = widget;
        self.rx = rx;
    }

    fn new_with_external(text: String, allow_external: bool) -> Self {
        let (widget, rx) = Self::build_widget(&text, allow_external);
        Self { text, widget, rx }
    }

    fn build_widget(text: &str, allow_external: bool) -> WidgetBuild {
        let cfg = global::config_sync();

        let rx = if allow_external {
            match cfg.ui.markdown_render_engine {
                MarkdownRenderEngine::ExternalCommand { executable, args } => {
                    let (tx, rx) = oneshot::channel();

                    tokio::task::spawn({
                        let text = text.to_string();
                        async move {
                            match ExternalMarkdownViewer::try_new(&text, &executable, &args).await {
                                Ok(widget) => {
                                    trace!("using an external CLI tool to render Markdown success");
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

        let widget = CodeHighlight::try_new(text, coco_highlight::Lang::Markdown)
            .map(|x| x.into())
            .unwrap_or_else(|err| {
                warn!(?err, "failed to new CodeHighlight Component");
                RawTextViewer::new(text.to_string()).into()
            });
        (widget, rx)
    }
}

impl Persistable for Plain {
    fn save(&self) -> Session {
        session::save(&self.text)
    }

    fn load(session: Session) -> Result<Self> {
        Ok(Self::new(session::load(session)?))
    }
}

impl Component for Plain {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        let children: Vec<&mut dyn Component> = vec![self.widget.deref_mut()];
        Box::new(children.into_iter())
    }

    fn on_cache_invalidation(&mut self, reason: CacheInvalidation) {
        self.widget.invalidate_cache(reason);
    }

    fn on_tick(&mut self) {
        if let Some(rx) = &mut self.rx
            && let Ok(widget) = rx.try_recv()
        {
            self.widget = widget;
            self.rx = None;
            global::signal_dirty();
            trace!("replaced inner widget of Plain Message");
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.widget.draw(frame, area)
    }
}

impl Content for Plain {
    fn height(&self, width: u16) -> usize {
        self.widget.height(width)
    }
}

impl ContentComponent for Plain {}
