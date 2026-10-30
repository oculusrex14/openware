//! Kernel errors.

#[derive(Debug)]
pub enum CoreError {
    Io(std::io::Error),
    Format(String),
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Io(e) => write!(f, "io: {e}"),
            CoreError::Format(m) => write!(f, "format: {m}"),
        }
    }
}

impl std::error::Error for CoreError {}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

