use crate::combo::Instruction;

pub fn parse_instructions(text: &str, command_prefix: &str) -> Vec<Instruction> {
    let mut instructions: Vec<Instruction> = vec![];

    macro_rules! push_text {
        ($block:expr) => {
            if let Some(last) = instructions.last_mut()
                && let Instruction::Command { output, .. } = last
                && output.is_empty()
            {
                output.push_str(&$block);
            } else {
                instructions.push(Instruction::Text($block));
            }
        };
    }

    let last = text.lines().fold(String::new(), |mut block, line| {
        if let Some(command) = line.strip_prefix(command_prefix) {
            push_text!(block);

            let command = command.trim().to_string();
            let inst = Instruction::Command {
                command,
                output: String::new(),
            };
            instructions.push(inst);
            String::new()
        } else {
            block.push_str(line);
            block.push('\n');
            block
        }
    });
    push_text!(last);

    instructions
}
