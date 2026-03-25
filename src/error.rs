use std::fmt;

pub const MEMORY_BUDGET_ERROR_PREFIX: &str = "MEMORY_BUDGET_EXCEEDED:";

#[derive(Debug)]
pub enum FoldError {
    Serialization(String),
    Deserialization(String),
    Io(std::io::Error),
    ConcurrentClaim(String),
    MemoryBudgetExceeded(String),
    Interner(String),
    Other(String),
}

impl fmt::Display for FoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FoldError::Serialization(e) => write!(f, "Serialization error: {}", e),
            FoldError::Deserialization(e) => write!(f, "Deserialization error: {}", e),
            FoldError::Io(e) => write!(f, "IO error: {}", e),
            FoldError::ConcurrentClaim(e) => write!(f, "Concurrent claim: {}", e),
            FoldError::MemoryBudgetExceeded(e) => write!(f, "Memory budget exceeded: {}", e),
            FoldError::Interner(e) => write!(f, "Interner error: {}", e),
            FoldError::Other(e) => write!(f, "Error: {}", e),
        }
    }
}

impl std::error::Error for FoldError {}

impl From<std::io::Error> for FoldError {
    fn from(err: std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::Other {
            let message = err.to_string();
            if let Some(rest) = message.strip_prefix(MEMORY_BUDGET_ERROR_PREFIX) {
                return FoldError::MemoryBudgetExceeded(rest.trim().to_string());
            }
        }
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
