use code_combo::MarkdownRenderEngine;
use ratatui::{Frame, prelude::Rect};
use tokio::sync::oneshot;
use tracing::{trace, warn};

use crate::{components::ContentComponent, global};

use super::{Component, Content};

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
pub struct Plain {
    widget: Box<dyn ContentComponent>,
    rx: Option<oneshot::Receiver<Box<dyn ContentComponent>>>,
}

impl Plain {
    pub fn new(text: String) -> Self {
        let cfg = global::config_sync();

        let rx = match cfg.ui.markdown_render_engine {
            MarkdownRenderEngine::ExternalCommand { executable, args } => {
                let (tx, rx) = oneshot::channel();

                tokio::task::spawn({
                    let text = text.clone();
                    async move {
                        match ExternalMarkdownViewer::try_new(&text, &executable, &args).await {
                            Ok(widget) => {
                                trace!("using an external CLI tool to render Markdown success");
                                tx.send(widget.boxed()).ok();
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

        let widget = RawTextViewer::new(text).boxed();
        Self { widget, rx }
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

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> color_eyre::eyre::Result<()> {
        self.widget.draw(frame, area)
    }
}

impl Content for Plain {
    fn height(&self, width: u16) -> usize {
        self.widget.height(width)
    }
}

impl ContentComponent for Plain {}
