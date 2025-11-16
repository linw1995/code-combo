use snafu::ResultExt;
use tree_sitter_highlight::HighlightConfiguration;

use super::Result;

#[derive(Debug, PartialEq)]
pub enum Lang {
    Bash,
    Markdown,
    MarkdownInline,
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        use Lang::*;
        match self {
            Bash => "bash",
            Markdown => "markdown",
            MarkdownInline => "markdown_inline",
        }
    }
}

impl std::fmt::Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn new_config(lang: &Lang) -> Result<(HighlightConfiguration, Vec<&'static str>)> {
    use Lang::*;
    match lang {
        Bash => bash_config(),
        _ => unimplemented!(),
    }
}

fn bash_config() -> Result<(HighlightConfiguration, Vec<&'static str>)> {
    let mut config = HighlightConfiguration::new(
        tree_sitter_bash::LANGUAGE.into(),
        "bash",
        tree_sitter_bash::HIGHLIGHT_QUERY,
        "",
        "",
    )
    .whatever_context("failed to create bash highlight configuration")?;

    let names = [
        "string", "function", "property", "keyword", "comment", "number", "embedded", "operator",
        "constant",
    ];
    config.configure(&names);

    Ok((config, Vec::from(names)))
}
