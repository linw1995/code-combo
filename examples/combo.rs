use anthropic_api::{Credentials, messages::*};
use serde_json::json;

use code_combo::{Instruction, execute_starter};

#[tokio::main]
async fn main() {
    let model = std::env::var("ANTHROPIC_MODEL").expect("ANTHROPIC_MODEL must be set");
    let credentials = Credentials::from_env();

    let bash_tool = Tool {
        name: "Bash".to_string(),
        description: "A Bash for excuting command".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command to execute"},
                "timeout": {"type": "number", "description": "Optional timeout in milliseconds", "max": 600000}
            },
            "required": ["command"]
        }),
    };

    let (execution, _) = execute_starter("./examples/commit.sh", true);
    let starter = execution.await.expect("execute_starter failed");
    let combo = starter.combo.unwrap();

    let content = format!(
        r#"
All the commands that you need have already been executed.

{}
"#,
        combo
            .instructions
            .into_iter()
            .map(|instruction| match instruction {
                Instruction::Text(text) => text,
                Instruction::Command { command, output } => {
                    format!("Command: {}\nOutput: {}", command, output)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    )
    .trim()
    .to_string();

    let mut messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Text(content),
    }];

    println!("Send messages: {:?}", messages);

    // Send message with tool
    let response = MessagesBuilder::builder(&model, messages.clone(), 1024)
        .credentials(credentials)
        .tools(vec![bash_tool])
        .tool_choice(ToolChoice::Any)
        .create()
        .await
        .unwrap();

    // Process response
    for content in response.content {
        match content {
            ResponseContentBlock::Text { text } => {
                println!("Assistant: {}", text.trim());
                messages.push(Message {
                    role: MessageRole::Assistant,
                    content: MessageContent::Text(text),
                });
            }
            ResponseContentBlock::ToolUse { name, input, .. } => {
                println!("Claude decided to use the tool: {}: {}", name, input);
            }
            ResponseContentBlock::Thinking {
                signature,
                thinking,
            } => {
                println!("Claude {} is thinking: {}", signature, thinking);
            }
            ResponseContentBlock::RedactedThinking { data } => {
                println!("Claude is thinking: {}", data);
            }
        }
    }
}
