use std::process::Stdio;

use ansi_to_tui::IntoText;
use coco_macro::{ComponentExt, ContentComponentExt};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Paragraph, Wrap},
};
use snafu::prelude::*;
use tokio::io::AsyncWriteExt;

use crate::{
    components::{Component, Content, ContentComponent, Persistable},
    error::*,
    session::Session,
};

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "external_markdown_viewer")]
pub struct ExternalMarkdownViewer<'a> {
    widget: Paragraph<'a>,
}

impl<'a> ExternalMarkdownViewer<'a> {
    pub async fn try_new(text: &str, cmd: &str, args: &[String]) -> Result<Self> {
        let output = match tokio::process::Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut cmd) => {
                let mut stdin = cmd.stdin.take().unwrap();
                match stdin.write(text.as_bytes()).await {
                    Ok(_) => {
                        drop(stdin);
                        cmd.wait_with_output().await
                    }
                    Err(err) => Err(err),
                }
            }
            Err(e) => Err(e),
        }
        .whatever_context("failed to run external markdown viewer")?;

        let text = output
            .stdout
            .into_text()
            .whatever_context("failed to convert the external markdown viewer result")?;
        let widget = Paragraph::new(text).wrap(Wrap { trim: false });
        Ok(Self { widget })
    }
}

impl Persistable for ExternalMarkdownViewer<'static> {
    fn save(&self) -> Session {
        unreachable!("External markdown viewer doesn't support saving session")
    }

    fn load(_session: Session) -> Result<Self> {
        unreachable!("External markdown viewer doesn't support loading session")
    }
}

impl Component for ExternalMarkdownViewer<'static> {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl<'a> Content for ExternalMarkdownViewer<'a> {
    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }
}

impl ContentComponent for ExternalMarkdownViewer<'static> {}
