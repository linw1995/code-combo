use crate::{Event::*, Lang, Result, highlight};
use indoc::indoc;

#[test]
#[snafu::report]
fn parse_markdown_with_injections() -> Result<()> {
    let source = indoc! {r#"
        Hello *world*.

        ```bash
        echo "hi"
        ```
    "#}
    .trim();

    let events = highlight(
        &Lang::Markdown,
        &[
            "text.emphasis",
            "text.literal",
            "punctuation.delimiter",
            "function",
            "string",
        ],
        source,
    )?;

    let has_emphasis = events
        .iter()
        .any(|event| matches!(event, Start("text.emphasis")));
    let has_bash = events
        .iter()
        .any(|event| matches!(event, Start("function")));
    assert!(has_emphasis, "expected markdown inline emphasis highlight");
    assert!(has_bash, "expected injected bash highlight");

    let merged = events
        .iter()
        .filter_map(|event| match event {
            Source(text) => Some(*text),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(merged, source);

    Ok(())
}
