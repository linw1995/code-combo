use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use tree_sitter_highlight::HighlightConfiguration;

use super::Result;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Lang {
    Bash,
    Diff,
    Json,
    Markdown,
    MarkdownInline,
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        use Lang::*;
        match self {
            Bash => "bash",
            Diff => "diff",
            Json => "json",
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
        Json => json_config(names),
        Markdown => markdown_config(names),
        MarkdownInline => markdown_inline_config(names),
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

fn json_config(names: &[&str]) -> Result<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(
        tree_sitter_json::LANGUAGE.into(),
        "json",
        tree_sitter_json::HIGHLIGHTS_QUERY,
        "",
        "",
    )
    .whatever_context("failed to create json highlight configuration")?;

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

fn markdown_inline_config(names: &[&str]) -> Result<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(
        tree_sitter_md::INLINE_LANGUAGE.into(),
        "markdown_inline",
        tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
        tree_sitter_md::INJECTION_QUERY_INLINE,
        "",
    )
    .whatever_context("failed to create markdown inline highlight configuration")?;

    config.configure(names);

    Ok(config)
}
