use std::collections::HashMap;

use lazy_static::lazy_static;
use ratatui::style::{Color as RatatuiColor, Modifier, Style as RatatuiStyle};
use serde::{Deserialize, Serialize};
use snafu::OptionExt;
use tracing::warn;

use crate::error;

#[derive(Clone, Serialize, Deserialize)]
pub struct Theme {
    scheme: ThemeScheme,
    palettes: ColorPalette,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ThemeScheme {
    ui: HashMap<String, Style>,
    tree_sitter: HashMap<String, Style>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Style {
    Advance(StyleAdvance),
    Fg(Color),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Color {
    Color(RatatuiColor),
    PaletteColor(String),
}

type ColorPalette = HashMap<String, RatatuiColor>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StyleAdvance {
    fg: Option<Color>,
    bg: Option<Color>,
    underline_color: Option<Color>,
    add_modifier: Option<String>,
    sub_modifier: Option<String>,
}

pub struct FinalizedTheme {
    pub ui: FinalizedUiTheme,
    pub tree_sitter: HashMap<String, RatatuiStyle>,
}

macro_rules! build_theme_type {
    ($struct_name:ident { $($field:ident),+ $(,)? }) => {
        pub struct $struct_name {
            $(pub $field: RatatuiStyle,)+
        }
    };
}

macro_rules! build_theme {
    ($self:ident, $struct_name:ident { $($field:ident),+ $(,)? }) => {
        {
            let resolve = |name| $self.resolve(name);
            Ok($struct_name {
                $($field: resolve(stringify!($field))?,)+
            })
        }
    };
}

build_theme_type!(FinalizedUiTheme {
    user_role,
    bot_role,
    shortcut,
    shortcut_desc,
    auto_accept_on,
    auto_accept_off,
    bash_tab_stdout,
    bash_tab_stderr,
    bash_tab_mixed,
    bash_tab_active,
    bash_stdout_marker,
    bash_stderr_marker,
    tab_spaces,
});

impl Theme {
    fn resolve(&self, name: &str) -> error::Result<RatatuiStyle> {
        let style = self
            .scheme
            .ui
            .get(name)
            .whatever_context("failed to get style from theme")?;
        style.to_ratatui(&self.palettes)
    }

    pub fn to_ratatui(&self) -> error::Result<FinalizedTheme> {
        let mut tree_sitter = HashMap::with_capacity(self.scheme.tree_sitter.len());
        for (name, style) in self.scheme.tree_sitter.iter() {
            tree_sitter.insert(name.to_owned(), style.to_ratatui(&self.palettes)?);
        }
        Ok(FinalizedTheme {
            ui: build_theme!(
                self,
                FinalizedUiTheme {
                    user_role,
                    bot_role,
                    shortcut,
                    shortcut_desc,
                    auto_accept_on,
                    auto_accept_off,
                    bash_tab_stdout,
                    bash_tab_stderr,
                    bash_tab_mixed,
                    bash_tab_active,
                    bash_stdout_marker,
                    bash_stderr_marker,
                    tab_spaces,
                }
            )?,
            tree_sitter,
        })
    }
}

impl Color {
    fn to_ratatui(&self, palettes: &ColorPalette) -> error::Result<RatatuiColor> {
        Ok(match self {
            Color::Color(color) => color.to_owned(),
            Color::PaletteColor(name) => palettes
                .get(name)
                .cloned()
                .whatever_context("failed to get color from palettes")?,
        })
    }
}

impl Style {
    fn to_ratatui(&self, palettes: &ColorPalette) -> error::Result<RatatuiStyle> {
        let StyleAdvance {
            fg,
            bg,
            underline_color,
            add_modifier,
            sub_modifier,
        } = match self {
            Style::Fg(fg) => StyleAdvance {
                fg: Some(fg.to_owned()),
                ..Default::default()
            },
            Style::Advance(value) => value.to_owned(),
        };

        let add_modifier = add_modifier
            .map(|name| match bitflags::parser::from_str(&name) {
                Err(err) => {
                    warn!(?name, ?err, "invalid add_modifier of style");
                    Modifier::empty()
                }
                Ok(v) => v,
            })
            .unwrap_or_default();

        let sub_modifier = sub_modifier
            .map(|name| match bitflags::parser::from_str(&name) {
                Err(err) => {
                    warn!(?name, ?err, "invalid sub_modifier of style");
                    Modifier::empty()
                }
                Ok(v) => v,
            })
            .unwrap_or_default();

        let fg = fg.map(|c| c.to_ratatui(palettes)).transpose()?;
        let bg = bg.map(|c| c.to_ratatui(palettes)).transpose()?;
        let underline_color = underline_color
            .map(|c| c.to_ratatui(palettes))
            .transpose()?;
        Ok(RatatuiStyle {
            fg,
            bg,
            underline_color,
            add_modifier,
            sub_modifier,
        })
    }
}

lazy_static! {
    pub static ref CATPPUCCIN_SCHEME: ThemeScheme =
        serde_json::from_str(include_str!("../theme/catppuccin_scheme.json"))
            .expect("failed to load catppuccin scheme");
    pub static ref CATPPUCCIN_MOCHA_PALLETE: ColorPalette =
        serde_json::from_str(include_str!("../theme/catppuccin_mocha_palette.json"))
            .expect("failed to load catppuccin mocha palette");
    pub static ref CATPPUCCIN_MOCHA_THEME: FinalizedTheme = {
        Theme {
            scheme: CATPPUCCIN_SCHEME.clone(),
            palettes: CATPPUCCIN_MOCHA_PALLETE.clone(),
        }
        .to_ratatui()
        .expect("failed to convert theme")
    };
}

pub fn use_builtin_theme(name: &str) -> &'static FinalizedTheme {
    match name {
        "catppuccin_mocha" => &CATPPUCCIN_MOCHA_THEME,
        _ => unreachable!("unknown theme: {name}"),
    }
}
