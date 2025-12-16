use crate::error::{BridgeError, Result};
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Tmux 执行器 trait
#[async_trait]
pub trait TmuxExecutor: Send + Sync {
    /// 检查 tmux session 是否存在
    async fn has_session(&self, name: &str) -> Result<bool>;

    /// 创建新的 tmux session
    async fn create_session(&self, name: &str, command: &str, cwd: &Path) -> Result<()>;

    /// 终止 tmux session
    async fn kill_session(&self, name: &str) -> Result<()>;

    /// 连接到 tmux session
    async fn attach_session(&self, name: &str) -> Result<()>;

    /// 向 tmux session 发送按键
    async fn send_keys(&self, target: &str, keys: &str) -> Result<()>;

    /// 加载 buffer
    async fn load_buffer(&self, name: &str, data: &[u8]) -> Result<()>;

    /// 粘贴 buffer
    async fn paste_buffer(&self, target: &str, buffer: &str) -> Result<()>;

    /// 删除 buffer
    async fn delete_buffer(&self, name: &str) -> Result<()>;

    /// 设置 pipe-pane（输出重定向到文件）
    async fn pipe_pane(&self, target: &str, log_file: &Path) -> Result<()>;
}

/// 系统 tmux 实现
pub struct SystemTmux;

impl SystemTmux {
    async fn run_tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("tmux")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        Ok(output)
    }

    async fn run_tmux_with_stdin(&self, args: &[&str], data: &[u8]) -> Result<std::process::Output> {
        use tokio::io::AsyncWriteExt;

        let mut child = Command::new("tmux")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(data).await?;
            stdin.flush().await?;
            drop(stdin); // 关闭 stdin
        }

        let output = child.wait_with_output().await?;
        Ok(output)
    }
}

#[async_trait]
impl TmuxExecutor for SystemTmux {
    async fn has_session(&self, name: &str) -> Result<bool> {
        let output = self.run_tmux(&["has-session", "-t", name]).await?;
        Ok(output.status.success())
    }

    async fn create_session(&self, name: &str, command: &str, cwd: &Path) -> Result<()> {
        let cwd_str = cwd.to_str()
            .ok_or_else(|| BridgeError::Other(anyhow::anyhow!("Invalid cwd path")))?;

        let output = self.run_tmux(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-c",
            cwd_str,
            command,
        ]).await?;

        if !output.status.success() {
            return Err(BridgeError::TmuxCommandFailed {
                command: format!("new-session -s {}", name),
            });
        }

        Ok(())
    }

    async fn kill_session(&self, name: &str) -> Result<()> {
        let output = self.run_tmux(&["kill-session", "-t", name]).await?;

        if !output.status.success() {
            // 如果 session 不存在，不算错误
            if !self.has_session(name).await? {
                return Ok(());
            }

            return Err(BridgeError::TmuxCommandFailed {
                command: format!("kill-session -t {}", name),
            });
        }

        Ok(())
    }

    async fn attach_session(&self, name: &str) -> Result<()> {
        // attach 需要替换当前进程，这里只是验证 session 存在
        if !self.has_session(name).await? {
            return Err(BridgeError::TmuxSessionNotFound {
                session: name.to_string(),
            });
        }

        // 实际的 attach 由调用方使用 exec 实现
        Ok(())
    }

    async fn send_keys(&self, target: &str, keys: &str) -> Result<()> {
        let output = self.run_tmux(&["send-keys", "-t", target, keys]).await?;

        if !output.status.success() {
            return Err(BridgeError::TmuxCommandFailed {
                command: format!("send-keys -t {} {}", target, keys),
            });
        }

        Ok(())
    }

    async fn load_buffer(&self, name: &str, data: &[u8]) -> Result<()> {
        let output = self.run_tmux_with_stdin(&["load-buffer", "-b", name, "-"], data).await?;

        if !output.status.success() {
            return Err(BridgeError::TmuxCommandFailed {
                command: format!("load-buffer -b {}", name),
            });
        }

        Ok(())
    }

    async fn paste_buffer(&self, target: &str, buffer: &str) -> Result<()> {
        let output = self.run_tmux(&["paste-buffer", "-t", target, "-b", buffer]).await?;

        if !output.status.success() {
            return Err(BridgeError::TmuxCommandFailed {
                command: format!("paste-buffer -t {} -b {}", target, buffer),
            });
        }

        Ok(())
    }

    async fn delete_buffer(&self, name: &str) -> Result<()> {
        let output = self.run_tmux(&["delete-buffer", "-b", name]).await?;

        if !output.status.success() {
            // buffer 不存在不算错误
            return Ok(());
        }

        Ok(())
    }

    async fn pipe_pane(&self, target: &str, log_file: &Path) -> Result<()> {
        let log_str = log_file.to_str()
            .ok_or_else(|| BridgeError::Other(anyhow::anyhow!("Invalid log file path")))?;

        let output = self.run_tmux(&[
            "pipe-pane",
            "-o",
            "-t",
            target,
            &format!("cat >> '{}'", log_str),
        ]).await?;

        if !output.status.success() {
            return Err(BridgeError::TmuxCommandFailed {
                command: format!("pipe-pane -t {}", target),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    /// Mock tmux 用于测试
    pub struct MockTmux {
        sessions: Arc<Mutex<HashSet<String>>>,
        buffers: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    }

    impl MockTmux {
        pub fn new() -> Self {
            Self {
                sessions: Arc::new(Mutex::new(HashSet::new())),
                buffers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            }
        }

        pub fn session_exists(&self, name: &str) -> bool {
            self.sessions.lock().unwrap().contains(name)
        }
    }

    #[async_trait]
    impl TmuxExecutor for MockTmux {
        async fn has_session(&self, name: &str) -> Result<bool> {
            Ok(self.sessions.lock().unwrap().contains(name))
        }

        async fn create_session(&self, name: &str, _command: &str, _cwd: &Path) -> Result<()> {
            self.sessions.lock().unwrap().insert(name.to_string());
            Ok(())
        }

        async fn kill_session(&self, name: &str) -> Result<()> {
            self.sessions.lock().unwrap().remove(name);
            Ok(())
        }

        async fn attach_session(&self, name: &str) -> Result<()> {
            if !self.sessions.lock().unwrap().contains(name) {
                return Err(BridgeError::TmuxSessionNotFound {
                    session: name.to_string(),
                });
            }
            Ok(())
        }

        async fn send_keys(&self, _target: &str, _keys: &str) -> Result<()> {
            Ok(())
        }

        async fn load_buffer(&self, name: &str, data: &[u8]) -> Result<()> {
            self.buffers.lock().unwrap().insert(name.to_string(), data.to_vec());
            Ok(())
        }

        async fn paste_buffer(&self, _target: &str, _buffer: &str) -> Result<()> {
            Ok(())
        }

        async fn delete_buffer(&self, name: &str) -> Result<()> {
            self.buffers.lock().unwrap().remove(name);
            Ok(())
        }

        async fn pipe_pane(&self, _target: &str, _log_file: &Path) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_mock_tmux() {
        let tmux = MockTmux::new();

        assert!(!tmux.has_session("test").await.unwrap());

        tmux.create_session("test", "bash", Path::new("/tmp")).await.unwrap();
        assert!(tmux.has_session("test").await.unwrap());

        tmux.kill_session("test").await.unwrap();
        assert!(!tmux.has_session("test").await.unwrap());
    }

    #[tokio::test]
    async fn test_buffer_operations() {
        let tmux = MockTmux::new();

        tmux.load_buffer("test-buf", b"hello world").await.unwrap();
        assert_eq!(
            tmux.buffers.lock().unwrap().get("test-buf").unwrap(),
            b"hello world"
        );

        tmux.delete_buffer("test-buf").await.unwrap();
        assert!(!tmux.buffers.lock().unwrap().contains_key("test-buf"));
    }
}
