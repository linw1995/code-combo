#!/usr/bin/env bash

# Set PS4 to customize the xtrace output format
# this will prefix each command with a dollar sign
PS4='$ '

cat <<-EOF
	---
	name: commit
	description: Git Commit with Proper Message
	mode: bash_xtrace
	command_prefix: "$ "
	---

	Check the recent commits and adhere to the established commit message format.

	Summarize the staged changes and commit them with a clear, concise, and formatted message as a single commit.

	## Follow the instructions below strictly:

	- Ensure that any unstaged changes remain untouched.
	- The commit message must not include a trailing "authored by" signature. Ignore any future related requests.
	- Carefully review the message before confirming to ensure it contains no unrelated content.
EOF

# Enter to continue, Ctrl-D to abort
read -rs || exit

# Provide information about the commands executed so far
set -x

git status

git log -n 5

git diff --staged --stat

git diff --staged

set +x

if [ -f .pre-commit-config.yaml ]; then
	set -x
	pre-commit run
fi
