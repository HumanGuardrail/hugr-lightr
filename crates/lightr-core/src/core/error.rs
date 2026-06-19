use super::digest::Digest;

#[derive(Debug)]
pub enum LightrError {
    NotFound(Digest),
    RefNotFound(String),
    Integrity {
        expected: Digest,
        actual: Digest,
    },
    TooLarge {
        size: u64,
        cap: u64,
    },
    InvalidRef(String),
    InvalidManifest(String),
    /// Registry/network protocol error (OCI pull), with the HTTP status.
    /// Distinct from Io so auth (401/403), not-found (404), rate-limit (429)
    /// and 5xx surface their own message instead of collapsing to "Io".
    Registry {
        status: u16,
        msg: String,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for LightrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LightrError::NotFound(d) => write!(f, "not found in store: {}", d.to_hex()),
            LightrError::RefNotFound(n) => write!(f, "ref not found: {n}"),
            LightrError::Integrity { expected, actual } => write!(
                f,
                "integrity violation: expected {} got {}",
                expected.to_hex(),
                actual.to_hex()
            ),
            LightrError::TooLarge { size, cap } => {
                write!(f, "blob of {size} bytes exceeds cap {cap}")
            }
            LightrError::InvalidRef(n) => write!(f, "invalid ref: {n}"),
            LightrError::InvalidManifest(msg) => write!(f, "invalid manifest: {msg}"),
            LightrError::Registry { status, msg } => {
                write!(f, "registry error (HTTP {status}): {msg}")
            }
            LightrError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LightrError {}

impl From<std::io::Error> for LightrError {
    fn from(e: std::io::Error) -> Self {
        LightrError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, LightrError>;
