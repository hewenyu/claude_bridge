/// Codex 通信模块
///
/// 提供与 Codex 交互的核心功能：
/// - log_reader: 日志文件监控和解析
/// - communicator: 高级通信 API
/// - bridge: FIFO 桥接守护进程（待实现）

pub mod communicator;
pub mod error;
pub mod log_reader;

pub use communicator::CodexCommunicator;
pub use error::{CodexError, Result};
pub use log_reader::{CodexLogReader, ReadState};
