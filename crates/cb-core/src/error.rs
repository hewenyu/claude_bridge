use std::path::PathBuf;
use thiserror::Error;

/// 主错误类型
#[derive(Error, Debug)]
pub enum BridgeError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Session unhealthy: {reason}")]
    SessionUnhealthy { reason: String },

    #[error("FIFO error at {path}: {source}")]
    FifoError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Tmux session {session} not found")]
    TmuxSessionNotFound { session: String },

    #[error("Tmux command failed: {command}")]
    TmuxCommandFailed { command: String },

    #[error("Process {pid} not running")]
    ProcessNotRunning { pid: i32 },

    #[error("Log file error: {0}")]
    LogError(#[from] LogReaderError),

    #[error("Timeout after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("Invalid session file format: {0}")]
    InvalidSessionFormat(String),

    #[error("Runtime directory not found: {0}")]
    RuntimeDirNotFound(PathBuf),

    #[error("Platform not supported: {0}")]
    UnsupportedPlatform(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// 日志读取器特定错误
#[derive(Error, Debug)]
pub enum LogReaderError {
    #[error("No log files found in {0}")]
    NoLogsFound(PathBuf),

    #[error("Invalid log format: {0}")]
    InvalidFormat(String),

    #[error("Session filter mismatch")]
    SessionFilterMismatch,

    #[error("Log file not accessible: {path}")]
    LogNotAccessible { path: PathBuf },
}

/// 结果类型别名
pub type Result<T> = std::result::Result<T, BridgeError>;
