/// Codex 通信错误类型
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CodexError>;

#[derive(Error, Debug)]
pub enum CodexError {
    #[error("日志文件未找到: {0}")]
    LogNotFound(String),

    #[error("FIFO 错误: {0}")]
    FifoError(String),

    #[error("超时: {0}")]
    Timeout(String),

    #[error("消息解析失败: {0}")]
    ParseError(String),

    #[error("会话未找到")]
    SessionNotFound,

    #[error("会话不健康: {0}")]
    SessionUnhealthy(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Bridge 错误: {0}")]
    Bridge(#[from] cb_core::BridgeError),

    #[error("其他错误: {0}")]
    Other(#[from] anyhow::Error),
}
