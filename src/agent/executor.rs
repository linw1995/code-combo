use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct Executor {}

#[derive(Debug, Serialize, Deserialize)]
pub struct BashInput {
    pub command: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout: u64,
}

fn default_timeout_ms() -> u64 {
    600_000
}

impl Executor {
    pub async fn execute(&self, name: &str, input: serde_json::Value) {
        match name {
            "Bash" => {
                let input: BashInput = serde_json::from_value(input).expect("Valid input");
                bash(input.command, input.timeout).await;
            }
            _ => unimplemented!("Unknown {name} tool"),
        }
    }
}

async fn bash(command: String, timeout: u64) {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout as tokio_timeout};

    match tokio_timeout(
        Duration::from_millis(timeout),
        Command::new("bash").arg("-c").arg(command).output(),
    )
    .await
    {
        Ok(Ok(output)) => {
            println!("Status: {}", output.status);
            println!("Stdout: {}", String::from_utf8_lossy(&output.stdout));
            eprintln!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(Err(e)) => {
            eprintln!("Failed to execute command: {}", e);
        }
        Err(_) => {
            eprintln!("Command timed out after {} ms", timeout);
        }
    }
}
