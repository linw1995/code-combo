use crate::{Event::Source, Lang, Result, highlight};
use indoc::indoc;

#[test]
#[snafu::report]
fn parse_json_roundtrip() -> Result<()> {
    let source = indoc! {r#"
        {
          "name": "alice",
          "age": 30,
          "active": true,
          "tags": ["a", "b"],
          "meta": {"score": 4.5, "note": null}
        }
    "#}
    .trim();

    let events = highlight(
        &Lang::Json,
        &["string", "number", "keyword", "property"],
        source,
    )?;
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
