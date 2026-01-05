use coco_macro::{ComponentExt, ContentComponentExt};
use code_combo::{Block as ChatBlock, Content as ChatContent, Message as ChatMessage};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    prelude::Rect,
    widgets::Wrap,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    components::{CodeHighlight, Component, Content, ContentComponent, Persistable},
    error::Result,
    session::{self, Session},
    widgets::Paragraph,
};

#[derive(Serialize, Deserialize)]
struct TranscriptState {
    message: ChatMessage,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_message")]
pub struct TranscriptMessage {
    state: TranscriptState,
    segments: Vec<Box<dyn ContentComponent>>,
}

const INDENT_STEP: u16 = 2;

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_plain")]
struct TranscriptPlain {
    text: String,
    widget: Paragraph<'static>,
}

impl TranscriptPlain {
    fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let widget = Paragraph::new_wrap(text.clone(), Wrap { trim: false });
        Self { text, widget }
    }
}

impl Persistable for TranscriptPlain {
    fn save(&self) -> Session {
        session::save(&self.text)
    }

    fn load(session: Session) -> Result<Self> {
        let text: String = session::load(session)?;
        Ok(Self::new(text))
    }
}

impl Component for TranscriptPlain {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.widget, area);
        Ok(())
    }
}

impl Content for TranscriptPlain {
    fn height(&self, width: u16) -> usize {
        self.widget.line_count(width)
    }
}

impl ContentComponent for TranscriptPlain {}

#[derive(Serialize, Deserialize)]
struct TranscriptTextState {
    text: String,
    detect_json: bool,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_text")]
struct TranscriptText {
    state: TranscriptTextState,
    content: Box<dyn ContentComponent>,
}

impl TranscriptText {
    fn new(text: &str, detect_json: bool) -> Self {
        let content = text_component(text, detect_json);
        Self {
            state: TranscriptTextState {
                text: text.to_string(),
                detect_json,
            },
            content,
        }
    }
}

impl Persistable for TranscriptText {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: TranscriptTextState = session::load(session)?;
        Ok(Self::new(&state.text, state.detect_json))
    }
}

impl Component for TranscriptText {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![self.content.as_mut() as &mut dyn Component].into_iter())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let header = "- type: text";
        let key = "text:";
        let header_height = line_height(header, area.width);
        let key_height = line_height(key, area.width.saturating_sub(INDENT_STEP));
        let content_height = child_height(self.content.as_ref(), area.width, INDENT_STEP * 2);
        let chunks = vertical_chunks(area, &[header_height, key_height, content_height]);
        if !chunks.is_empty() {
            draw_line(frame, chunks[0], header);
        }
        if chunks.len() >= 2 {
            draw_line(frame, indented_area(chunks[1], INDENT_STEP), key);
        }
        if chunks.len() >= 3 {
            draw_child(self.content.as_mut(), frame, chunks[2], INDENT_STEP * 2)?;
        }
        Ok(())
    }
}

impl Content for TranscriptText {
    fn height(&self, width: u16) -> usize {
        let header = "- type: text";
        let key = "text:";
        let header_height = line_height(header, width);
        let key_height = line_height(key, width.saturating_sub(INDENT_STEP));
        let content_height = child_height(self.content.as_ref(), width, INDENT_STEP * 2);
        header_height + key_height + content_height
    }
}

impl ContentComponent for TranscriptText {}

#[derive(Serialize, Deserialize)]
struct TranscriptToolUseState {
    tool_use: code_combo::ToolUse,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_tool_use")]
struct TranscriptToolUse {
    state: TranscriptToolUseState,
    input: Box<dyn ContentComponent>,
}

impl TranscriptToolUse {
    fn new(tool_use: &code_combo::ToolUse) -> Self {
        let input = json_component(&tool_use.input);
        Self {
            state: TranscriptToolUseState {
                tool_use: tool_use.clone(),
            },
            input,
        }
    }
}

impl Persistable for TranscriptToolUse {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: TranscriptToolUseState = session::load(session)?;
        Ok(Self::new(&state.tool_use))
    }
}

impl Component for TranscriptToolUse {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![self.input.as_mut() as &mut dyn Component].into_iter())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let header = "- type: tool_use";
        let id = format!("id: {}", self.state.tool_use.id);
        let name = format!("name: {}", self.state.tool_use.name);
        let input_key = "input:";

        let header_height = line_height(header, area.width);
        let id_height = line_height(&id, area.width.saturating_sub(INDENT_STEP));
        let name_height = line_height(&name, area.width.saturating_sub(INDENT_STEP));
        let input_height = line_height(input_key, area.width.saturating_sub(INDENT_STEP));
        let body_height = child_height(self.input.as_ref(), area.width, INDENT_STEP * 2);
        let chunks = vertical_chunks(
            area,
            &[
                header_height,
                id_height,
                name_height,
                input_height,
                body_height,
            ],
        );

        if !chunks.is_empty() {
            draw_line(frame, chunks[0], header);
        }
        if chunks.len() >= 2 {
            draw_line(frame, indented_area(chunks[1], INDENT_STEP), &id);
        }
        if chunks.len() >= 3 {
            draw_line(frame, indented_area(chunks[2], INDENT_STEP), &name);
        }
        if chunks.len() >= 4 {
            draw_line(frame, indented_area(chunks[3], INDENT_STEP), input_key);
        }
        if chunks.len() >= 5 {
            draw_child(self.input.as_mut(), frame, chunks[4], INDENT_STEP * 2)?;
        }
        Ok(())
    }
}

impl Content for TranscriptToolUse {
    fn height(&self, width: u16) -> usize {
        let header = "- type: tool_use";
        let id = format!("id: {}", self.state.tool_use.id);
        let name = format!("name: {}", self.state.tool_use.name);
        let input_key = "input:";
        let header_height = line_height(header, width);
        let id_height = line_height(&id, width.saturating_sub(INDENT_STEP));
        let name_height = line_height(&name, width.saturating_sub(INDENT_STEP));
        let input_height = line_height(input_key, width.saturating_sub(INDENT_STEP));
        let body_height = child_height(self.input.as_ref(), width, INDENT_STEP * 2);
        header_height + id_height + name_height + input_height + body_height
    }
}

impl ContentComponent for TranscriptToolUse {}

#[derive(Serialize, Deserialize)]
struct TranscriptToolResultState {
    tool_use_id: String,
    is_error: Option<bool>,
    content: ChatContent,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_tool_result")]
struct TranscriptToolResult {
    state: TranscriptToolResultState,
    content: TranscriptContentSection,
}

impl TranscriptToolResult {
    fn new(tool_use_id: &str, is_error: Option<bool>, content: &ChatContent) -> Self {
        let content_section = TranscriptContentSection::new(content, true);
        Self {
            state: TranscriptToolResultState {
                tool_use_id: tool_use_id.to_string(),
                is_error,
                content: content.clone(),
            },
            content: content_section,
        }
    }
}

impl Persistable for TranscriptToolResult {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: TranscriptToolResultState = session::load(session)?;
        Ok(Self::new(
            &state.tool_use_id,
            state.is_error,
            &state.content,
        ))
    }
}

impl Component for TranscriptToolResult {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![&mut self.content as &mut dyn Component].into_iter())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let header = "- type: tool_result";
        let tool_use_id = format!("tool_use_id: {}", self.state.tool_use_id);
        let mut heights = Vec::with_capacity(5);
        heights.push(line_height(header, area.width));
        heights.push(line_height(
            &tool_use_id,
            area.width.saturating_sub(INDENT_STEP),
        ));
        if let Some(is_error) = self.state.is_error {
            let is_error = format!("is_error: {is_error}");
            heights.push(line_height(
                &is_error,
                area.width.saturating_sub(INDENT_STEP),
            ));
        }
        let content_key = "content:";
        heights.push(line_height(
            content_key,
            area.width.saturating_sub(INDENT_STEP),
        ));
        heights.push(child_height(&self.content, area.width, INDENT_STEP * 2));

        let chunks = vertical_chunks(area, &heights);
        let mut idx = 0;
        if let Some(chunk) = chunks.get(idx) {
            draw_line(frame, *chunk, header);
        }
        idx += 1;
        if let Some(chunk) = chunks.get(idx) {
            draw_line(frame, indented_area(*chunk, INDENT_STEP), &tool_use_id);
        }
        idx += 1;
        if let Some(is_error) = self.state.is_error {
            if let Some(chunk) = chunks.get(idx) {
                draw_line(
                    frame,
                    indented_area(*chunk, INDENT_STEP),
                    &format!("is_error: {is_error}"),
                );
            }
            idx += 1;
        }
        if let Some(chunk) = chunks.get(idx) {
            draw_line(frame, indented_area(*chunk, INDENT_STEP), content_key);
        }
        idx += 1;
        if let Some(chunk) = chunks.get(idx) {
            draw_child(&mut self.content, frame, *chunk, INDENT_STEP * 2)?;
        }
        Ok(())
    }
}

impl Content for TranscriptToolResult {
    fn height(&self, width: u16) -> usize {
        let header = "- type: tool_result";
        let tool_use_id = format!("tool_use_id: {}", self.state.tool_use_id);
        let mut total = 0;
        total += line_height(header, width);
        total += line_height(&tool_use_id, width.saturating_sub(INDENT_STEP));
        if let Some(is_error) = self.state.is_error {
            total += line_height(
                &format!("is_error: {is_error}"),
                width.saturating_sub(INDENT_STEP),
            );
        }
        total += line_height("content:", width.saturating_sub(INDENT_STEP));
        total += child_height(&self.content, width, INDENT_STEP * 2);
        total
    }
}

impl ContentComponent for TranscriptToolResult {}

#[derive(Serialize, Deserialize)]
struct TranscriptBlocksState {
    blocks: Vec<ChatBlock>,
    detect_json: bool,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_blocks")]
struct TranscriptBlocks {
    state: TranscriptBlocksState,
    items: Vec<Box<dyn ContentComponent>>,
}

impl TranscriptBlocks {
    fn new(blocks: &[ChatBlock], detect_json: bool) -> Self {
        let items = build_block_items(blocks, detect_json);
        Self {
            state: TranscriptBlocksState {
                blocks: blocks.to_vec(),
                detect_json,
            },
            items,
        }
    }
}

impl Persistable for TranscriptBlocks {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: TranscriptBlocksState = session::load(session)?;
        Ok(Self::new(&state.blocks, state.detect_json))
    }
}

impl Component for TranscriptBlocks {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(
            self.items
                .iter_mut()
                .map(|item| item.as_mut() as &mut dyn Component),
        )
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if self.items.is_empty() {
            return Ok(());
        }
        let heights = self
            .items
            .iter()
            .map(|item| item.height(area.width))
            .collect::<Vec<_>>();
        let chunks = vertical_chunks(area, &heights);
        for (item, chunk) in self.items.iter_mut().zip(chunks.iter()) {
            item.draw(frame, *chunk)?;
        }
        Ok(())
    }
}

impl Content for TranscriptBlocks {
    fn height(&self, width: u16) -> usize {
        self.items.iter().map(|item| item.height(width)).sum()
    }
}

impl ContentComponent for TranscriptBlocks {}

#[derive(Serialize, Deserialize)]
struct TranscriptBlocksSectionState {
    content: ChatContent,
    detect_json: bool,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_blocks_section")]
struct TranscriptBlocksSection {
    state: TranscriptBlocksSectionState,
    blocks: TranscriptBlocks,
}

impl TranscriptBlocksSection {
    fn new(content: &ChatContent, detect_json: bool) -> Self {
        let blocks = blocks_from_content(content);
        let blocks = TranscriptBlocks::new(&blocks, detect_json);
        Self {
            state: TranscriptBlocksSectionState {
                content: content.clone(),
                detect_json,
            },
            blocks,
        }
    }
}

impl Persistable for TranscriptBlocksSection {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: TranscriptBlocksSectionState = session::load(session)?;
        Ok(Self::new(&state.content, state.detect_json))
    }
}

impl Component for TranscriptBlocksSection {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![&mut self.blocks as &mut dyn Component].into_iter())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let label = "blocks:";
        let label_height = line_height(label, area.width);
        let blocks_height = child_height(&self.blocks, area.width, INDENT_STEP);
        let chunks = vertical_chunks(area, &[label_height, blocks_height]);
        if !chunks.is_empty() {
            draw_line(frame, chunks[0], label);
        }
        if chunks.len() >= 2 {
            draw_child(&mut self.blocks, frame, chunks[1], INDENT_STEP)?;
        }
        Ok(())
    }
}

impl Content for TranscriptBlocksSection {
    fn height(&self, width: u16) -> usize {
        let label_height = line_height("blocks:", width);
        let blocks_height = child_height(&self.blocks, width, INDENT_STEP);
        label_height + blocks_height
    }
}

impl ContentComponent for TranscriptBlocksSection {}

#[derive(Serialize, Deserialize)]
struct TranscriptContentSectionState {
    content: ChatContent,
    detect_json: bool,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "transcript_content_section")]
struct TranscriptContentSection {
    state: TranscriptContentSectionState,
    label: String,
    content: Box<dyn ContentComponent>,
}

impl TranscriptContentSection {
    fn new(chat_content: &ChatContent, detect_json: bool) -> Self {
        let (label, content_component) = match chat_content {
            ChatContent::Text(text) => ("text".to_string(), text_component(text, detect_json)),
            ChatContent::Multiple(blocks) => (
                "blocks".to_string(),
                TranscriptBlocks::new(blocks, detect_json).into(),
            ),
        };
        Self {
            state: TranscriptContentSectionState {
                content: chat_content.clone(),
                detect_json,
            },
            label,
            content: content_component,
        }
    }
}

impl Persistable for TranscriptContentSection {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: TranscriptContentSectionState = session::load(session)?;
        Ok(Self::new(&state.content, state.detect_json))
    }
}

impl Component for TranscriptContentSection {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        Box::new(vec![self.content.as_mut() as &mut dyn Component].into_iter())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let label = format!("{}:", self.label);
        let label_height = line_height(&label, area.width);
        let content_height = child_height(self.content.as_ref(), area.width, INDENT_STEP);
        let chunks = vertical_chunks(area, &[label_height, content_height]);
        if !chunks.is_empty() {
            draw_line(frame, chunks[0], &label);
        }
        if chunks.len() >= 2 {
            draw_child(self.content.as_mut(), frame, chunks[1], INDENT_STEP)?;
        }
        Ok(())
    }
}

impl Content for TranscriptContentSection {
    fn height(&self, width: u16) -> usize {
        let label = format!("{}:", self.label);
        let label_height = line_height(&label, width);
        let content_height = child_height(self.content.as_ref(), width, INDENT_STEP);
        label_height + content_height
    }
}

impl ContentComponent for TranscriptContentSection {}

impl TranscriptMessage {
    pub fn new(message: ChatMessage) -> Self {
        let segments = build_segments(&message);
        Self {
            state: TranscriptState { message },
            segments,
        }
    }
}

fn build_segments(message: &ChatMessage) -> Vec<Box<dyn ContentComponent>> {
    let role = match message.role {
        code_combo::Role::User => "user",
        code_combo::Role::Assistant => "assistant",
    };
    vec![
        TranscriptPlain::new(format!("role: {role}")).into(),
        TranscriptBlocksSection::new(&message.content, false).into(),
    ]
}

fn blocks_from_content(content: &ChatContent) -> Vec<ChatBlock> {
    match content {
        ChatContent::Text(text) => vec![ChatBlock::Text { text: text.clone() }],
        ChatContent::Multiple(blocks) => blocks.clone(),
    }
}

fn build_block_items(blocks: &[ChatBlock], detect_json: bool) -> Vec<Box<dyn ContentComponent>> {
    blocks
        .iter()
        .map(|block| block_component(block, detect_json))
        .collect()
}

fn block_component(block: &ChatBlock, detect_json: bool) -> Box<dyn ContentComponent> {
    match block {
        ChatBlock::Text { text } => TranscriptText::new(text, detect_json).into(),
        ChatBlock::ToolUse(tool_use) => TranscriptToolUse::new(tool_use).into(),
        ChatBlock::ToolResult {
            tool_use_id,
            is_error,
            content,
        } => TranscriptToolResult::new(tool_use_id, *is_error, content).into(),
    }
}

fn line_height(text: &str, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    Paragraph::new_wrap(text.to_string(), Wrap { trim: false }).line_count(width)
}

fn draw_line(frame: &mut Frame, area: Rect, text: &str) {
    if area.width == 0 {
        return;
    }
    let widget = Paragraph::new_wrap(text.to_string(), Wrap { trim: false });
    frame.render_widget(widget, area);
}

fn indented_area(area: Rect, indent: u16) -> Rect {
    if area.width <= indent {
        return Rect::new(area.x + area.width, area.y, 0, area.height);
    }
    Rect::new(area.x + indent, area.y, area.width - indent, area.height)
}

fn child_height(child: &dyn ContentComponent, width: u16, indent: u16) -> usize {
    let width = width.saturating_sub(indent);
    if width == 0 { 0 } else { child.height(width) }
}

fn draw_child(
    child: &mut dyn ContentComponent,
    frame: &mut Frame,
    area: Rect,
    indent: u16,
) -> Result<()> {
    let area = indented_area(area, indent);
    if area.width == 0 {
        return Ok(());
    }
    child.draw(frame, area)
}

fn vertical_chunks(area: Rect, heights: &[usize]) -> Vec<Rect> {
    if heights.is_empty() {
        return Vec::new();
    }
    let constraints = heights
        .iter()
        .map(|height| Constraint::Length(*height as u16))
        .collect::<Vec<_>>();
    Layout::vertical(constraints).split(area).to_vec()
}

fn text_component(text: &str, detect_json: bool) -> Box<dyn ContentComponent> {
    if detect_json && let Ok(value) = serde_json::from_str::<Value>(text) {
        return json_component(&value);
    }
    markdown_component(text)
}

fn markdown_component(text: &str) -> Box<dyn ContentComponent> {
    if text.is_empty() {
        return TranscriptPlain::new(String::new()).into();
    }
    match CodeHighlight::try_new(text, coco_highlight::Lang::Markdown) {
        Ok(widget) => widget.into(),
        Err(_) => TranscriptPlain::new(text).into(),
    }
}

fn json_component(value: &Value) -> Box<dyn ContentComponent> {
    let json = serde_json::to_string_pretty(value).unwrap_or_else(|_| "[Invalid JSON]".into());
    match CodeHighlight::try_new(&json, coco_highlight::Lang::Json) {
        Ok(widget) => widget.into(),
        Err(_) => TranscriptPlain::new(json).into(),
    }
}

impl Persistable for TranscriptMessage {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: TranscriptState = session::load(session)?;
        Ok(Self::new(state.message))
    }
}

impl Component for TranscriptMessage {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if self.segments.is_empty() {
            return Ok(());
        }

        let heights = self
            .segments
            .iter()
            .map(|segment| segment.height(area.width))
            .collect::<Vec<_>>();
        let chunks = vertical_chunks(area, &heights);
        for (segment, chunk) in self.segments.iter_mut().zip(chunks.iter()) {
            segment.draw(frame, chunk.to_owned())?;
        }
        Ok(())
    }
}

impl Content for TranscriptMessage {
    fn height(&self, width: u16) -> usize {
        self.segments
            .iter()
            .map(|segment| segment.height(width))
            .sum()
    }
}

impl ContentComponent for TranscriptMessage {}
