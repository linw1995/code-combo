use snafu::prelude::*;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(whatever, display("{message}"))]
    Whatever {
        message: String,
        #[snafu(source(from(Box<dyn std::error::Error + Send + Sync + 'static>, Some)))]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
        backtrace: snafu::Backtrace,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
