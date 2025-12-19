use ratatui::{
    prelude::{Buffer, Rect},
    style::Style,
    text::Span,
    widgets::StatefulWidget,
};

#[derive(Clone, Copy, Debug)]
pub struct Set {
    pub full: &'static str,
    pub empty: &'static str,
    pub symbols: &'static [&'static str],
}

pub const BRAILLE_EIGHT_DOUBLE: Set = Set {
    full: "⣿",
    empty: "　",
    symbols: &["⣧", "⣏", "⡟", "⠿", "⢻", "⣹", "⣼", "⣶"],
};

#[derive(Clone, Debug)]
pub struct Throbber {
    set: Set,
    style: Style,
}

impl Default for Throbber {
    fn default() -> Self {
        Self {
            set: BRAILLE_EIGHT_DOUBLE,
            style: Style::default(),
        }
    }
}

impl Throbber {
    pub fn throbber_set(mut self, set: Set) -> Self {
        self.set = set;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn to_symbol_span(&self, state: &ThrobberState) -> Span<'static> {
        Span::styled(state.current_symbol(&self.set), self.style)
    }
}

impl StatefulWidget for Throbber {
    type State = ThrobberState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.is_empty() {
            return;
        }

        let symbol = state.current_symbol(&self.set);
        buf[(area.x, area.y)]
            .set_symbol(symbol)
            .set_style(self.style);
    }
}

#[derive(Clone, Debug, Default)]
pub struct ThrobberState {
    frame: usize,
}

impl ThrobberState {
    pub fn calc_next(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    fn current_symbol(&self, set: &Set) -> &'static str {
        if set.symbols.is_empty() {
            return "";
        }
        let idx = self.frame % set.symbols.len();
        set.symbols[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_symbols() {
        let mut state = ThrobberState::default();
        let throbber = Throbber::default().throbber_set(BRAILLE_EIGHT_DOUBLE);
        let expected = BRAILLE_EIGHT_DOUBLE
            .symbols
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>();

        let mut got = Vec::new();
        for _ in 0..BRAILLE_EIGHT_DOUBLE.symbols.len() {
            got.push(throbber.to_symbol_span(&state).content.to_string());
            state.calc_next();
        }

        assert_eq!(got, expected);

        // second cycle
        got.clear();
        for _ in 0..BRAILLE_EIGHT_DOUBLE.symbols.len() {
            got.push(throbber.to_symbol_span(&state).content.to_string());
            state.calc_next();
        }
        assert_eq!(got, expected);
    }
}
