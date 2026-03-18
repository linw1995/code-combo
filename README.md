# CoCo

[![codecov](https://codecov.io/gh/linw1995/code-combo/graph/badge.svg?token=GKGKYNQ1VM)](https://codecov.io/gh/linw1995/code-combo)
![GitHub Actions Workflow Status](https://img.shields.io/github/actions/workflow/status/linw1995/code-combo/CI.yaml)

The Code Combo You Need.

**English** | [简体中文](README.zh-CN.md)

> [!CAUTION]
> Work in Progress

![demo video](https://github.com/user-attachments/assets/80f2ab11-9312-424b-9a2d-e0e5ac89cdb5)

Due to the stochasticity of LLM generation, this project aims to solidify
common usage patterns to improve the determinism and inference efficiency
of results.
Through the provided `coco` CLI, operations that originally required
multiple turns of conversation can be executed directly.
LLM inference is invoked only when necessary.
Ideally, only one turn is required to obtain the expected results,
significantly improving inference efficiency.
This solidification of LLM usage patterns is called a **Combo Script**.

The project also supports converting MCPs (Model Context Protocol) into CLI
commands, thereby offloading MCPs from the LLM context.
Instead, relevant MCP CLI commands are discovered via sub-agents.
This also enables Combo Scripts to invoke MCPs.

## Example

```sh
#!/usr/bin/env nix-shell
#!nix-shell -i bash -p docopts

set -Ee

coco metadata name=commit description="Code commit" \
    thinking=on thinking_budget=2048 || exit 0

DOC="
Usage:
    commit [options]

Options:
  --context <context>   The context of background.
  -h --help             Show this screen.
"

eval "$(docopts -h "$DOC" : "$@")"

if [ -f .pre-commit-config.yaml ]; then
        coco record git status --short
        on_err() {
                coco tell <<EOF
If it's a formatting type check failure, it has most likely been
automatically fixed by the checks.

After resolving the issue, you need to re-add the fixed files and re-execute.
EOF
        }
        trap on_err ERR
        if command -v prek &>/dev/null; then
                coco record prek run
        elif command -v pre-commit &>/dev/null; then
                coco record pre-commit run
        fi
        trap - ERR
fi

coco record git log -n 5

coco record git status

coco record git diff --staged --stat

coco record git diff --staged

context="Context: $context"
resp=$(
        coco ask --schemas 'message:git commit message' <<EOF
# Summarize the staged changes and write an appropriate git commit message

$context

- The language and format of the message should follow previous commits
- The message should not contain '#' + number or '@' + username unless required
- The summarized git commit message should be concise and professional
EOF
)

message=$(jq -r '.message' <<<"$resp")
escaped=$(printf '%q' "$message")
coco record "git commit -n -F - <<< $escaped"
```

## Usage

See [Wiki](https://github.com/linw1995/code-combo/wiki)
