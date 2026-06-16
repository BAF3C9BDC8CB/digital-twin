use std::fmt;

#[derive(Debug)]
pub enum DtError {
    Neo4j(String),
    Qdrant(String),
    Embed(String),
    Sqlite(String),
    Parse(String),
    Config(String),
}

impl fmt::Display for DtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DtError::Neo4j(m) => write!(f, "Neo4j error: {}", m),
            DtError::Qdrant(m) => write!(f, "Qdrant error: {}", m),
            DtError::Embed(m) => write!(f, "Embed error: {}", m),
            DtError::Sqlite(m) => write!(f, "SQLite error: {}", m),
            DtError::Parse(m) => write!(f, "Parse error: {}", m),
            DtError::Config(m) => write!(f, "Config error: {}", m),
        }
    }
}

impl std::error::Error for DtError {}

#[macro_export]
macro_rules! warn_on_err {
    ($expr:expr, $ctx:expr) => {
        if let Err(e) = $expr {
            eprintln!("[warn] {}: {}", $ctx, e);
        }
    };
}
