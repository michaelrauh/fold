use std::fmt;

#[derive(Debug)]
pub enum FoldError {
    Database(String),
    Queue(String),
    Serialization(Box<bincode::error::EncodeError>),
    Deserialization(Box<bincode::error::DecodeError>),
    Io(std::io::Error),
    Interner(String),
    Other(String),
}

impl fmt::Display for FoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FoldError::Database(e) => write!(f, "Database error: {}", e),
            FoldError::Queue(e) => write!(f, "Queue error: {}", e),
            FoldError::Serialization(e) => write!(f, "Serialization error: {}", e),
            FoldError::Deserialization(e) => write!(f, "Deserialization error: {}", e),
            FoldError::Io(e) => write!(f, "IO error: {}", e),
            FoldError::Interner(e) => write!(f, "Interner error: {}", e),
            FoldError::Other(e) => write!(f, "Error: {}", e),
        }
    }
}

impl std::error::Error for FoldError {}

impl From<Box<bincode::error::EncodeError>> for FoldError {
    fn from(err: Box<bincode::error::EncodeError>) -> Self {
        FoldError::Serialization(err)
    }
}

impl From<bincode::error::EncodeError> for FoldError {
    fn from(err: bincode::error::EncodeError) -> Self {
        FoldError::Serialization(Box::new(err))
    }
}

impl From<Box<bincode::error::DecodeError>> for FoldError {
    fn from(err: Box<bincode::error::DecodeError>) -> Self {
        FoldError::Deserialization(err)
    }
}

impl From<bincode::error::DecodeError> for FoldError {
    fn from(err: bincode::error::DecodeError) -> Self {
        FoldError::Deserialization(Box::new(err))
    }
}

impl From<std::io::Error> for FoldError {
    fn from(err: std::io::Error) -> Self {
        FoldError::Io(err)
    }
}

impl From<String> for FoldError {
    fn from(err: String) -> Self {
        FoldError::Other(err)
    }
}

impl From<&str> for FoldError {
    fn from(err: &str) -> Self {
        FoldError::Other(err.to_string())
    }
}