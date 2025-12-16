pub mod daemon;
pub mod error;
pub mod pty;
pub mod session;

// 保留 tmux 模块用于向后兼容（已废弃）
#[deprecated(note = "Use pty module instead")]
pub mod tmux;

pub use error::{BridgeError, LogReaderError, Result};
pub use pty::{PtyConfig, PtyExecutor, SystemPty};
pub use daemon::{SessionDaemon, DaemonClient, DaemonRequest, DaemonResponse};

#[cfg(test)]
pub use pty::tests::MockPty;
