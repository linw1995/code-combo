use snafu::prelude::*;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("Parser for language '{}' not found", lang))]
    MissingParser { lang: String },

    #[snafu(display("Canonicalizer for language '{}' not found", lang))]
    MissingCanonicalizer { lang: String },

    #[snafu(whatever, display("{message}"))]
    Whatever {
        message: String,
        #[snafu(source(from(Box<dyn std::error::Error + Send + Sync + 'static>, Some)))]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
