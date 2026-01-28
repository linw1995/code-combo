//! Shared tab panel utilities for tool widgets.

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
