// ChronicleError variants are wired into callers progressively across stories.
// Allow dead-code until US-015/US-017/US-018 complete the wiring.
#![allow(dead_code)]

pub mod ring_buffer;

/// All error categories that Chronicle can produce.
///
/// These map directly to the seven categories in §11.1 of the spec and are
/// used as the `category` field in every error-ring-buffer entry.
#[derive(Debug, thiserror::Error)]
pub enum ChronicleError {
    /// Remote advanced during push and all retries were exhausted.
    #[error("push conflict: {message}")]
    PushConflict { message: String },

    /// A JSONL line could not be parsed.
    #[error("malformed line in {file}:{line}: {snippet}")]
    MalformedLine {
        file: String,
        line: usize,
        snippet: String,
    },

    /// Common entries differ between the local and remote copies (append-only
    /// invariant violated).
    #[error("prefix mismatch in {file}: {detail}")]
    PrefixMismatch { file: String, detail: String },

    /// Path canonicalization or de-canonicalization failed.
    #[error("canonicalization error for {path}: {message}")]
    CanonicalizationError { path: String, message: String },

    /// A git2 operation failed (network, auth, etc.).
    #[error("git error: {0}")]
    GitError(#[from] git2::Error),

    /// A filesystem read or write failed.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// A write failed because the disk is full.
    #[error("disk full while writing {path}")]
    DiskFull { path: String },
}

impl ChronicleError {
    /// Returns the stable category string used in the error ring buffer.
    #[must_use]
    pub fn category(&self) -> &'static str {
        match self {
            Self::PushConflict { .. } => "push_conflict",
            Self::MalformedLine { .. } => "malformed_line",
            Self::PrefixMismatch { .. } => "prefix_mismatch",
            Self::CanonicalizationError { .. } => "canonicalization_error",
            Self::GitError(_) => "git_error",
            Self::IoError(_) => "io_error",
            Self::DiskFull { .. } => "disk_full",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ChronicleError;

    #[test]
    fn category_strings_match_spec() {
        // Verify every category maps to the string defined in §11.1.
        let push = ChronicleError::PushConflict {
            message: String::from("test"),
        };
        assert_eq!(push.category(), "push_conflict");

        let malformed = ChronicleError::MalformedLine {
            file: String::from("f"),
            line: 1,
            snippet: String::from("x"),
        };
        assert_eq!(malformed.category(), "malformed_line");

        let mismatch = ChronicleError::PrefixMismatch {
            file: String::from("f"),
            detail: String::from("d"),
        };
        assert_eq!(mismatch.category(), "prefix_mismatch");

        let canon = ChronicleError::CanonicalizationError {
            path: String::from("p"),
            message: String::from("m"),
        };
        assert_eq!(canon.category(), "canonicalization_error");

        let disk = ChronicleError::DiskFull {
            path: String::from("p"),
        };
        assert_eq!(disk.category(), "disk_full");
    }

    #[test]
    fn io_error_wraps_correctly() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err = ChronicleError::IoError(io);
        assert_eq!(err.category(), "io_error");
        assert!(err.to_string().contains("IO error"));
    }
}
