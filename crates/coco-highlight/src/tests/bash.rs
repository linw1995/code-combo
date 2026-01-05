use crate::{Event::*, Lang, Result, highlight};
use indoc::indoc;
use tracing::debug;

#[test]
#[snafu::report]
fn parse_bash_simple() -> Result<()> {
    let source = indoc! {r#"
            echo 'hello world'
        "#}
    .trim();

    let events = highlight(&Lang::Bash, &["function", "string"], source)?;
    assert_eq!(
        events,
        vec![
            Start("function"),
            Source("echo"),
            End,
            Source(" "),
            Start("string"),
            Source("'hello world'"),
            End
        ]
    );

    Ok(())
}

#[test]
#[snafu::report]
fn parse_bash_shopts() -> Result<()> {
    let source = indoc! {r#"
            set -euo pipefail
        "#}
    .trim();
    let events = highlight(&Lang::Bash, &["function", "constant"], source)?;
    assert_eq!(
        events,
        vec![
            Start("function"),
            Source("set"),
            End,
            Source(" "),
            Start("constant"),
            Source("-euo"),
            End,
            Source(" pipefail")
        ]
    );
    Ok(())
}

#[test]
#[snafu::report]
fn parse_bash_variable_and_arrays() -> Result<()> {
    let source = indoc! {r#"
            NAME="world"
            readonly PI=3.14

            arr=(a b c)
            assoc=([foo]=1 [bar]=2)

            echo "Hello, $NAME! PI=$PI"
            echo "Array first: ${arr[0]}"
            echo "Assoc foo: ${assoc[foo]}"
        "#}
    .trim();
    let events = highlight(
        &Lang::Bash,
        &["property", "string", "function", "operator", "embedded"],
        source,
    )?;
    assert_eq!(
        events,
        vec![
            Start("property"),
            Source("NAME"),
            End,
            Source("="),
            Start("string"),
            Source("\"world\""),
            End,
            Source("\nreadonly "),
            Start("property"),
            Source("PI"),
            End,
            Source("=3.14\n\n"),
            Start("property"),
            Source("arr"),
            End,
            Source("=(a b c)\n"),
            Start("property"),
            Source("assoc"),
            End,
            Source("=([foo]=1 [bar]=2)\n\n"),
            Start("function"),
            Source("echo"),
            End,
            Source(" "),
            Start("string"),
            Source("\"Hello, "),
            Start("operator"),
            Source("$"),
            End,
            Start("property"),
            Source("NAME"),
            End,
            Source("! PI="),
            Start("operator"),
            Source("$"),
            End,
            Start("property"),
            Source("PI"),
            End,
            Source("\""),
            End,
            Source("\n"),
            Start("function"),
            Source("echo"),
            End,
            Source(" "),
            Start("string"),
            Source("\"Array first: "),
            Start("embedded"),
            Source("${"),
            Start("property"),
            Source("arr"),
            End,
            Source("[0]}"),
            End,
            Source("\""),
            End,
            Source("\n"),
            Start("function"),
            Source("echo"),
            End,
            Source(" "),
            Start("string"),
            Source("\"Assoc foo: "),
            Start("embedded"),
            Source("${"),
            Start("property"),
            Source("assoc"),
            End,
            Source("[foo]}"),
            End,
            Source("\""),
            End
        ]
    );
    Ok(())
}

#[test]
#[snafu::report]
fn parse_bash_full() -> Result<()> {
    let source = indoc! {r#"
            ########################################
            # Parameter expansion
            ########################################
            var=""
            echo "Default value: ${var:-default}"
            echo "Length: ${#NAME}"
            echo "Substring: ${NAME:0:3}"

            ########################################
            # Conditionals + test + [[ ]]
            ########################################
            if [[ -f "/etc/passwd" ]]; then
                echo "/etc/passwd exists"
            fi

            x=3
            if (( x > 2 )); then
                echo "x > 2"
            fi

            case "$NAME" in
                w*) echo "starts with w" ;;
                *)  echo "others" ;;
            esac

            ########################################
            # Loops
            ########################################
            for i in "${arr[@]}"; do
                echo "for-loop $i"
            done

            i=0
            while (( i < 3 )); do
                echo "while $i"
                ((i++))
            done

            ########################################
            # Functions + return + local variables
            ########################################
            foo() {
                local a=$1
                echo "func arg: $a"
                return 0
            }
            foo "hello func"

            ########################################
            # Command substitution, subshells,
            # pipes, and redirection
            ########################################
            now=$(date +%s)
            echo "epoch=$now"

            ( echo "subshell"; cd /; pwd )

            echo "line1" | tr a-z A-Z > out.txt

            ########################################
            # Here-doc
            ########################################
            cat <<'EOF' > heredoc.txt
            This is a here-doc.
            Var like $NAME won't expand because of quotes.
            EOF

            ########################################
            # trap example
            ########################################
            cleanup() {
                echo "Cleaning up..."
            }
            trap cleanup EXIT

            ########################################
            # Check commands and use test logic
            ########################################
            if command -v curl >/dev/null; then
                echo "curl exists"
            fi

            ########################################
            # Extended globbing
            ########################################
            shopt -s extglob
            case file.txt in
                *.@(txt|md)) echo "text-like";;
            esac

            echo "Done."
            "#};

    let events = highlight(&Lang::Bash, &[], source)?;
    debug!(?events, "highlight success");

    Ok(())
}
