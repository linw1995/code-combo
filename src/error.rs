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

pub trait ResultDisplayExt<T> {
    fn whatever_context_display(self, context: impl std::fmt::Display) -> Result<T>;
}

impl<T, E> ResultDisplayExt<T> for std::result::Result<T, E>
where
    E: std::fmt::Display,
{
    fn whatever_context_display(self, context: impl std::fmt::Display) -> Result<T> {
        let context = context.to_string();
        self.map_err(|err| {
            <Error as snafu::FromString>::without_source(format!("{context}: {err}"))
        })
    }
}
