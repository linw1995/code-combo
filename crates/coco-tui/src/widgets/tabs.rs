//! Shared tab panel utilities for tool widgets.

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::widgets::Paragraph;

/// Calculate the width for a vertical tab panel.
///
/// Returns 0 if total width is too narrow, otherwise returns the fixed panel width.
pub fn tab_panel_width(total_width: u16) -> u16 {
    const MIN_TOTAL_WIDTH: u16 = 24;
    const PANEL_WIDTH: u16 = 3;

    if total_width < MIN_TOTAL_WIDTH {
        0
    } else {
        PANEL_WIDTH
    }
}

/// Height of a tab panel with the given number of items.
pub const fn tab_panel_height(item_count: usize) -> usize {
    item_count
}

/// Renders a vertical tab panel with the given items.
///
/// # Arguments
/// * `current_index` - The index of the currently selected tab
/// * `items` - Array of (label, base_style) pairs
/// * `highlight` - Style to apply to the selected tab
pub fn render_tabs_panel(
    current_index: usize,
    items: &[(&str, Style)],
    highlight: Style,
) -> Paragraph<'static> {
    let lines = items
        .iter()
        .enumerate()
        .map(|(idx, (label, base_style))| {
            let style = if idx == current_index {
                highlight
            } else {
                Style::default()
            };
            Line::from(Span::styled((*label).to_string(), base_style.patch(style)))
        })
        .collect::<Vec<_>>();
    Paragraph::new(lines)
}
