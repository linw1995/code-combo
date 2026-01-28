use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use tree_sitter_highlight::HighlightConfiguration;

use super::Result;

#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize)]
pub enum Lang {
    Bash,
    Diff,
    Json,
    Markdown,
    MarkdownInline,
}

const MARKDOWN_INJECTIONS: &[Lang] = &[Lang::MarkdownInline, Lang::Bash, Lang::Diff, Lang::Json];

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

    pub fn from_injection_language(language: &str) -> Option<Self> {
        let language = language.trim();
        if language.eq_ignore_ascii_case("bash")
            || language.eq_ignore_ascii_case("sh")
            || language.eq_ignore_ascii_case("shell")
        {
            return Some(Lang::Bash);
        }
        if language.eq_ignore_ascii_case("diff") || language.eq_ignore_ascii_case("patch") {
            return Some(Lang::Diff);
        }
        if language.eq_ignore_ascii_case("json") || language.eq_ignore_ascii_case("jsonc") {
            return Some(Lang::Json);
        }
        if language.eq_ignore_ascii_case("markdown") || language.eq_ignore_ascii_case("md") {
            return Some(Lang::Markdown);
        }
        if language.eq_ignore_ascii_case("markdown_inline")
            || language.eq_ignore_ascii_case("markdown-inline")
            || language.eq_ignore_ascii_case("md-inline")
        {
            return Some(Lang::MarkdownInline);
        }
        None
    }

    pub fn injection_candidates(&self) -> &'static [Lang] {
        match self {
            Lang::Markdown => MARKDOWN_INJECTIONS,
            _ => &[],
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
        Bash => simple_config(
            tree_sitter_bash::LANGUAGE.into(),
            "bash",
            tree_sitter_bash::HIGHLIGHT_QUERY,
            names,
        ),
        Diff => simple_config(
            tree_sitter_diff::LANGUAGE.into(),
            "diff",
            tree_sitter_diff::HIGHLIGHTS_QUERY,
            names,
        ),
        Json => simple_config(
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
            names,
        ),
        Markdown => markdown_config(names),
        MarkdownInline => markdown_inline_config(names),
    }
}

fn simple_config(
    language: tree_sitter::Language,
    name: &str,
    highlights_query: &str,
    names: &[&str],
) -> Result<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(language, name, highlights_query, "", "")
        .whatever_context(format!("failed to create {name} highlight configuration"))?;
    config.configure(names);
    Ok(config)
}

fn markdown_config(names: &[&str]) -> Result<HighlightConfiguration> {
    let injection_query = markdown_injection_query();
    let mut config = HighlightConfiguration::new(
        tree_sitter_md::LANGUAGE.into(),
        "markdown",
        tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
        &injection_query,
        "",
    )
    .whatever_context("failed to create markdown highlight configuration")?;

    config.configure(names);

    Ok(config)
}

fn markdown_injection_query() -> String {
    let mut lines = Vec::new();
    for line in tree_sitter_md::INJECTION_QUERY_BLOCK.lines() {
        if line.contains("injection.language \"markdown_inline\"")
            && !line.contains("injection.include-children")
        {
            lines.push(format!("{line} (#set! injection.include-children)"));
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
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
