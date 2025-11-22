use indoc::indoc;

use crate::{Lang, highlight};

#[test]
fn parse_diff_simple() {
    let diff = indoc! {"
        --- a/file.txt
        +++ b/file.txt
        @@ -1,3 +1,4 @@
         line 1
        -line 2
        +line 2 modified
         line 3
        +line 4
    "};

    let result = highlight(&Lang::Diff, &["added", "removed", "context"], diff).unwrap();
    assert!(!result.is_empty());
}

#[test]
fn parse_diff_with_context() {
    let diff = indoc! {"
        diff --git a/src/main.rs b/src/main.rs
        index 1234567..abcdefg 100644
        --- a/src/main.rs
        +++ b/src/main.rs
        @@ -10,7 +10,7 @@ fn main() {
             let x = 5;
             let y = 10;

        -    println!(\"x + y = {}\", x + y);
        +    println!(\"x + y = {}\", x + y + 1);

             for i in 0..5 {
                 println!(\"i = {}\", i);
    "};

    let result = highlight(&Lang::Diff, &["added", "removed", "context"], diff).unwrap();
    assert!(!result.is_empty());
}

#[test]
fn parse_diff_unified() {
    let diff = indoc! {"
        --- old.txt
        +++ new.txt
        @@ -1,5 +1,5 @@
         context line 1
         context line 2
        -removed line
        +added line
         context line 3
         context line 4
    "};

    let result = highlight(&Lang::Diff, &["added", "removed", "context"], diff).unwrap();
    assert!(!result.is_empty());
}
