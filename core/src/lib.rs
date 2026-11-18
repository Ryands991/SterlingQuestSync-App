//! Sterling Quest Sync app core: everything except the window.

// Sterling code style puts a blank line between "}" and "else" on purpose.
#![allow(clippy::suspicious_else_formatting)]

pub mod api;
pub mod companion;
pub mod engine;
pub mod formats;
pub mod game;
pub mod keystore;
pub mod lua;
pub mod paths;
pub mod safety;
pub mod updater;
pub mod notices;

#[derive(Debug, thiserror::Error)]
pub enum Error
{
    #[error("refused: {0}")]
    Refused(String),
    #[error("couldn't read saved data: {0}")]
    Parse(String),
    #[error("unexpected format: {0}")]
    Format(String),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("server error: {0}")]
    Server(String),
    #[error("{}", crate::api::explain(code))]
    Api { status: u16, code: String, body: String },
    #[error("can't reach the server")]
    Offline,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
