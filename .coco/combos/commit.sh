#!/usr/bin/env bash

set -Ee

coco metadata name=commit description="Git Commit with Proper Message" || exit 0

if [ -f .pre-commit-config.yaml ]; then
	on_err() {
		coco tell <<EOF
If formatting-type checks fail, they have likely been automatically fixed.
You only need to re-add the fixed files and re-run the checks.
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

coco record git status

coco record git log -n 5

coco record git diff --staged --stat

coco record git diff --staged

resp=$(
	coco ask --schemas 'message:git commit message' <<EOF
# Summarize the staged changes and commit an appropriate message using git

- Use the language and message format consistent with previous commits
- The summarized git commit message should be concise, clear, and professional
EOF
)

message=$(jq -r '.message' <<<"$resp")
escaped=$(printf '%q' "$message")
coco record "git commit -F - <<< $escaped"
