mod error;
mod highlight;
mod lang;

#[cfg(test)]
mod tests;

pub use error::*;
pub use highlight::*;
pub use lang::*;

#[cfg(test)]
#[ctor::ctor]
fn init() {
    use std::io;
    use tracing_subscriber::{EnvFilter, prelude::*};

    let console_log = tracing_subscriber::fmt::layer()
        .pretty()
        .with_writer(io::stdout)
        .boxed();

    tracing_subscriber::registry()
        .with(vec![console_log])
        .with(EnvFilter::from_default_env())
        .init();
}
