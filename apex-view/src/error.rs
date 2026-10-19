use std::fmt;

/// Why a document or a page couldn't be shown.
#[derive(Debug)]
pub enum Error {
    /// The file is protected with a password.
    Encrypted,
    /// Not a format this build reads (or a feature of it left out).
    Unsupported,
    /// The file is damaged or not what its name says.
    Malformed(String),
    /// Past one of the [`crate::Limits`].
    TooLarge,
    /// The cancel callback asked to stop.
    Cancelled,
    /// A page number past the last page.
    NoSuchPage,
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Encrypted => f.write_str("the file is protected with a password"),
            Error::Unsupported => f.write_str("the file's format is not supported"),
            Error::Malformed(m) => write!(f, "the file is damaged: {m}"),
            Error::TooLarge => f.write_str("the file is too large to show"),
            Error::Cancelled => f.write_str("cancelled"),
            Error::NoSuchPage => f.write_str("no such page"),
            Error::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}
