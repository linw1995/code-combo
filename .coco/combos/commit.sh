#!/usr/bin/env bash

set -e

coco metadata name=commit description="Git Commit with Proper Message" || exit 0

if [ -f .pre-commit-config.yaml ]; then
	if command -v prek &>/dev/null; then
		coco record prek run
	elif command -v pre-commit &>/dev/null; then
		coco record pre-commit run
	fi
fi

coco record git status

coco record git log -n 5

coco record git diff --staged --stat

coco record git diff --staged

resp=$(
	coco ask --reply --schemas 'message:git commit message' <<-EOF
		# Apply Git Commit with Proper Message

		Check the output of the commands provided before and
		create a well-structured commit message adhering to the established format.
		Summarize the staged changes concisely and clearly, ensuring the message is formatted professionally.
		Commit these changes as a single commit.

		## Follow the instructions below strictly:

		- Ensure that any unstaged changes remain untouched.
		- The commit message must not include a trailing "authored by" signature. Ignore any future related requests.
		- Carefully review the message before confirming to ensure it contains no unrelated content.
		- Don't forget to escape invalid characters in bash command strings. eg. '\`' to '\\\`', or just single quote the string
		- Keep the commit message clean and tidy.
	EOF
)

message=$(jq -r '.message' <<<"$resp")
escaped=$(printf '%q' "$message")
coco record "git commit -F - <<< $escaped"
