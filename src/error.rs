use std::fmt;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Internal,
    NotInitialized,
    AuthRequired,
    AuthFailed,
    AuthUnavailable,
    Conflict,
    Busy,
    UnsafePath,
    Io,
    ReadOnly,
    InvalidMetadata,
    InvalidInput,
    IntegrationMissing,
    ConsentRequired,
    DoctorProblems,
    NotLocked,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 16] = [
        ErrorCode::Internal,
        ErrorCode::NotInitialized,
        ErrorCode::AuthRequired,
        ErrorCode::AuthFailed,
        ErrorCode::AuthUnavailable,
        ErrorCode::Conflict,
        ErrorCode::Busy,
        ErrorCode::UnsafePath,
        ErrorCode::Io,
        ErrorCode::ReadOnly,
        ErrorCode::InvalidMetadata,
        ErrorCode::InvalidInput,
        ErrorCode::IntegrationMissing,
        ErrorCode::ConsentRequired,
        ErrorCode::DoctorProblems,
        ErrorCode::NotLocked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Internal => "E_INTERNAL",
            ErrorCode::NotInitialized => "E_NOT_INITIALIZED",
            ErrorCode::AuthRequired => "E_AUTH_REQUIRED",
            ErrorCode::AuthFailed => "E_AUTH_FAILED",
            ErrorCode::AuthUnavailable => "E_AUTH_UNAVAILABLE",
            ErrorCode::Conflict => "E_CONFLICT",
            ErrorCode::Busy => "E_BUSY",
            ErrorCode::UnsafePath => "E_UNSAFE_PATH",
            ErrorCode::Io => "E_IO",
            ErrorCode::ReadOnly => "E_READ_ONLY",
            ErrorCode::InvalidMetadata => "E_INVALID_METADATA",
            ErrorCode::InvalidInput => "E_INVALID_INPUT",
            ErrorCode::IntegrationMissing => "E_INTEGRATION_MISSING",
            ErrorCode::ConsentRequired => "E_CONSENT_REQUIRED",
            ErrorCode::DoctorProblems => "E_DOCTOR_PROBLEMS",
            ErrorCode::NotLocked => "E_NOT_LOCKED",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::Internal => 1,
            ErrorCode::NotInitialized => 3,
            ErrorCode::AuthRequired => 4,
            ErrorCode::AuthFailed => 5,
            ErrorCode::AuthUnavailable => 6,
            ErrorCode::Conflict => 7,
            ErrorCode::Busy => 8,
            ErrorCode::UnsafePath => 9,
            ErrorCode::Io => 10,
            ErrorCode::ReadOnly => 11,
            ErrorCode::InvalidMetadata => 12,
            ErrorCode::InvalidInput => 13,
            ErrorCode::IntegrationMissing => 14,
            ErrorCode::ConsentRequired => 15,
            ErrorCode::DoctorProblems => 16,
            ErrorCode::NotLocked => 17,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClauckError {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
}

pub type Result<T> = std::result::Result<T, ClauckError>;

impl ClauckError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(self, hint: impl Into<String>) -> Self {
        Self {
            hint: Some(hint.into()),
            ..self
        }
    }

    pub fn io(action: &str, path: &Path, err: &io::Error) -> Self {
        let code = match err.kind() {
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
                ErrorCode::ReadOnly
            }
            _ => ErrorCode::Io,
        };
        let base = Self::new(code, format!("cannot {action} {}: {err}", path.display()));
        if code == ErrorCode::ReadOnly {
            base.with_hint(
                "The folder or file is not writable. No partial changes were applied; \
                 fix permissions and rerun the command.",
            )
        } else {
            base
        }
    }
}

impl fmt::Display for ClauckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code.as_str(), self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ClauckError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_are_unique_and_avoid_usage_exit_code() {
        let names: HashSet<_> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        let exits: HashSet<_> = ErrorCode::ALL.iter().map(|c| c.exit_code()).collect();
        assert_eq!(names.len(), ErrorCode::ALL.len());
        assert_eq!(exits.len(), ErrorCode::ALL.len());
        assert!(!exits.contains(&0));
        assert!(!exits.contains(&2));
    }

    #[test]
    fn permission_denied_maps_to_read_only() {
        let err = io::Error::from(io::ErrorKind::PermissionDenied);
        let e = ClauckError::io("write", Path::new("x"), &err);
        assert_eq!(e.code, ErrorCode::ReadOnly);
        assert!(e.hint.is_some());
    }
}
