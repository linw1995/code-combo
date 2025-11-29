use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::MarkdownRenderEngine;
use ratatui::{Frame, prelude::Rect};
use tokio::sync::oneshot;
use tracing::{trace, warn};

use super::{Component, Content};
use crate::{
    components::{CodeHighlight, ContentComponent, Persistable},
    error::*,
    global,
    session::{self, Session},
};

mod external_viewer;
mod raw;
use external_viewer::ExternalMarkdownViewer;
use raw::RawTextViewer;

/// Plain text render widget.
///
/// TODO: Support Markdown syntax with multiple approaches:
/// - Use a built-in Markdown parser and renderer. (Streaming)
/// - Use an external CLI tool to render Markdown
/// - Save content to a temporary file and open with external viewer
#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "plain")]
pub struct Plain {
    text: String,
    widget: Box<dyn ContentComponent>,
    rx: Option<oneshot::Receiver<Box<dyn ContentComponent>>>,
}

impl Plain {
    pub fn new(text: String) -> Self {
        let cfg = global::config_sync();

        // '\t' rendering doesn't work well in ratatui.
        // It causes the screen to retain the previous render result in the area of `\t` during scrolling.
        let text = text.replace("\t", "  ");

        let rx = match cfg.ui.markdown_render_engine {
            MarkdownRenderEngine::ExternalCommand { executable, args } => {
                let (tx, rx) = oneshot::channel();

                tokio::task::spawn({
                    let text = text.clone();
                    async move {
                        match ExternalMarkdownViewer::try_new(&text, &executable, &args).await {
                            Ok(widget) => {
                                trace!("using an external CLI tool to render Markdown success");
                                tx.send(widget.into()).ok();
                            }
                            Err(err) => {
                                warn!(?err, "failed using an external CLI tool to render Markdown");
                            }
                        };
                    }
                });

                Some(rx)
            }
            MarkdownRenderEngine::Native => None,
        };

        let widget = CodeHighlight::try_new(&text, code_highlight::Lang::Markdown)
            .map(|x| x.into())
            .unwrap_or_else(|err| {
                warn!(?err, "failed to new CodeHighlight Component");
                RawTextViewer::new(text.clone()).into()
            });
        Self { text, widget, rx }
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
    fn on_tick(&mut self) {
        if let Some(rx) = &mut self.rx {
            let Ok(widget) = rx.try_recv() else {
                return;
            };
            self.widget = widget;
            self.rx = None;
            global::signal_ditry();
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
