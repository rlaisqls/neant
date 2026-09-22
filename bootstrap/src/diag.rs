//! One error type for every phase. A position and a sentence.

#[derive(Debug, Clone)]
pub struct Error {
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn err<T>(line: u32, col: u32, msg: impl Into<String>) -> Result<T> {
    Err(Error { line, col, msg: msg.into() })
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}
