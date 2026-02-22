# CoCo

[![codecov](https://codecov.io/gh/linw1995/code-combo/graph/badge.svg?token=GKGKYNQ1VM)](https://codecov.io/gh/linw1995/code-combo)
![GitHub Actions Workflow Status](https://img.shields.io/github/actions/workflow/status/linw1995/code-combo/CI.yaml)

The Code Combo You Need.

> [!CAUTION]
> 开发中

由于 LLM 的输出存在不确定性，本项目旨在固化常用使用场景，以提高其结果的确定性及执行效率。
通过提供的 coco CLI 命令，将原本需要多轮对话才能调用的命令直接执行。
仅在需要 LLM 参与时才发起对话。
理想情况下，只需一轮对话即可获得预期结果，从而大幅提升执行效率。
这种固化 LLM 使用场景的成果，我称之为 Combo Script。

项目还支持将 MCP 转换为 CLI 命令，从而将 MCP 从 LLM
上下文中卸载。
转为通过子代理来发现任务所需的相关 MCP CLI 命令。
这也使得 Combo Script 能够调用 MCP。

## 示例

```sh
#!/usr/bin/env nix-shell
#!nix-shell -i bash -p docopts

set -Ee

coco metadata name=commit description="代码提交" \
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
如果是格式化类型 checks 失败的话，大概率已经被 checks 自动修复了。

问题解决后，需要重新添加修复后的文件，再重新执行
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

context="上下文：$context"
resp=$(
        coco ask --schemas 'message:git commit message' <<EOF
# 总结缓存的变更，并使用 git 提交合适的消息

$context

- 消息所使用语言及格式需要参考之前的提交
- 消息不能包含 '#' + 数字 和 '@' + 用户名 ，除非有要求
- 总结的 git commit 消息内容需简明扼要，及专业
EOF
)

message=$(jq -r '.message' <<<"$resp")
escaped=$(printf '%q' "$message")
coco record "git commit -n -F - <<< $escaped"
```

## 使用方式

见 [Wiki](https://github.com/linw1995/code-combo/wiki)
