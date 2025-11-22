use snafu::prelude::*;
use tree_sitter_highlight::HighlightConfiguration;

use super::Result;

#[derive(Debug, PartialEq)]
pub enum Lang {
    Bash,
    Diff,
    Markdown,
    MarkdownInline,
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        use Lang::*;
        match self {
            Bash => "bash",
            Diff => "diff",
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

pub fn new_config(lang: &Lang, names: &[&str]) -> Result<HighlightConfiguration> {
    use Lang::*;
    match lang {
        Bash => bash_config(names),
        Diff => diff_config(names),
        Markdown => markdown_config(names),
        _ => unimplemented!(),
    }
}

fn bash_config(names: &[&str]) -> Result<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(
        tree_sitter_bash::LANGUAGE.into(),
        "bash",
        tree_sitter_bash::HIGHLIGHT_QUERY,
        "",
        "",
    )
    .whatever_context("failed to create bash highlight configuration")?;

    config.configure(names);

    Ok(config)
}

fn diff_config(names: &[&str]) -> Result<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(
        tree_sitter_diff::LANGUAGE.into(),
        "diff",
        tree_sitter_diff::HIGHLIGHTS_QUERY,
        "",
        "",
    )
    .whatever_context("failed to create diff highlight configuration")?;

    config.configure(names);

    Ok(config)
}

fn markdown_config(names: &[&str]) -> Result<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(
        tree_sitter_md::LANGUAGE.into(),
        "markdown",
        tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
        tree_sitter_md::INJECTION_QUERY_BLOCK,
        "",
    )
    .whatever_context("failed to create markdown highlight configuration")?;

    config.configure(names);

    Ok(config)
}
