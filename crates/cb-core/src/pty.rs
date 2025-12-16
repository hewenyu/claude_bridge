use crate::error::{BridgeError, Result};
use async_trait::async_trait;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize, PtySystem};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// PTY 会话配置
#[derive(Debug, Clone)]
pub struct PtyConfig {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
        }
    }
}

/// PTY 执行器 trait
#[async_trait]
pub trait PtyExecutor: Send + Sync {
    /// 创建新的 PTY session
    async fn spawn_session(
        &self,
        name: &str,
        command: &str,
        args: &[&str],
        cwd: &Path,
        config: PtyConfig,
    ) -> Result<String>; // 返回 session ID

    /// 检查 session 是否存在
    async fn has_session(&self, session_id: &str) -> Result<bool>;

    /// 向 session 发送输入
    async fn send_input(&self, session_id: &str, data: &str) -> Result<()>;

    /// 从 session 读取输出（非阻塞）
    async fn read_output(&self, session_id: &str) -> Result<Option<String>>;

    /// 终止 session
    async fn kill_session(&self, session_id: &str) -> Result<()>;

    /// 获取 session 的 PID
    async fn get_pid(&self, session_id: &str) -> Result<u32>;
}

/// 系统 PTY 实现
pub struct SystemPty {
    sessions: Arc<Mutex<std::collections::HashMap<String, PtySessionHandle>>>,
}

struct PtySessionHandle {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output_rx: mpsc::UnboundedReceiver<String>,
    _reader_task: tokio::task::JoinHandle<()>,
}

impl SystemPty {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
}

impl Default for SystemPty {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PtyExecutor for SystemPty {
    async fn spawn_session(
        &self,
        name: &str,
        command: &str,
        args: &[&str],
        cwd: &Path,
        config: PtyConfig,
    ) -> Result<String> {
        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| BridgeError::Other(anyhow::anyhow!("Failed to open PTY: {}", e)))?;

        let mut cmd = CommandBuilder::new(command);
        cmd.args(args);
        cmd.cwd(cwd);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| BridgeError::Other(anyhow::anyhow!("Failed to spawn command: {}", e)))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| BridgeError::Other(anyhow::anyhow!("Failed to clone reader: {}", e)))?;

        let writer = pair.master.take_writer().map_err(|e| {
            BridgeError::Other(anyhow::anyhow!("Failed to take writer: {}", e))
        })?;

        // 启动异步读取任务
        let (output_tx, output_rx) = mpsc::unbounded_channel();
        let reader_task = tokio::task::spawn_blocking(move || {
            let mut reader = reader;
            let mut buffer = [0u8; 4096];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buffer[..n]).to_string();
                        if output_tx.send(text).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let session_id = format!("{}-{}", name, child.process_id().unwrap_or(0));

        let handle = PtySessionHandle {
            _master: pair.master,
            child,
            writer: Arc::new(Mutex::new(writer)),
            output_rx,
            _reader_task: reader_task,
        };

        self.sessions.lock().unwrap().insert(session_id.clone(), handle);

        Ok(session_id)
    }

    async fn has_session(&self, session_id: &str) -> Result<bool> {
        let sessions = self.sessions.lock().unwrap();
        Ok(sessions.contains_key(session_id))
    }

    async fn send_input(&self, session_id: &str, data: &str) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let handle = sessions
            .get(session_id)
            .ok_or_else(|| BridgeError::SessionNotFound(session_id.to_string()))?;

        let mut writer = handle.writer.lock().unwrap();
        writer
            .write_all(data.as_bytes())
            .map_err(|e| BridgeError::Io(e))?;
        writer
            .write_all(b"\n")
            .map_err(|e| BridgeError::Io(e))?;
        writer.flush().map_err(|e| BridgeError::Io(e))?;

        Ok(())
    }

    async fn read_output(&self, session_id: &str) -> Result<Option<String>> {
        let mut sessions = self.sessions.lock().unwrap();
        let handle = sessions
            .get_mut(session_id)
            .ok_or_else(|| BridgeError::SessionNotFound(session_id.to_string()))?;

        // 非阻塞读取
        match handle.output_rx.try_recv() {
            Ok(output) => Ok(Some(output)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Ok(None),
        }
    }

    async fn kill_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(mut handle) = sessions.remove(session_id) {
            let _ = handle.child.kill();
            let _ = handle.child.wait();
        }
        Ok(())
    }

    async fn get_pid(&self, session_id: &str) -> Result<u32> {
        let sessions = self.sessions.lock().unwrap();
        let handle = sessions
            .get(session_id)
            .ok_or_else(|| BridgeError::SessionNotFound(session_id.to_string()))?;

        handle
            .child
            .process_id()
            .ok_or_else(|| BridgeError::Other(anyhow::anyhow!("Failed to get process ID")))
    }
}

/// 测试模块
#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    /// Mock PTY 实现用于测试
    pub struct MockPty {
        sessions: Arc<RwLock<HashMap<String, MockSession>>>,
        next_pid: Arc<Mutex<u32>>,
    }

    struct MockSession {
        pid: u32,
        input_log: Vec<String>,
        output_queue: Vec<String>,
    }

    impl MockPty {
        pub fn new() -> Self {
            Self {
                sessions: Arc::new(RwLock::new(HashMap::new())),
                next_pid: Arc::new(Mutex::new(1000)),
            }
        }

        pub async fn get_input_log(&self, session_id: &str) -> Vec<String> {
            let sessions = self.sessions.read().await;
            sessions
                .get(session_id)
                .map(|s| s.input_log.clone())
                .unwrap_or_default()
        }

        pub async fn queue_output(&self, session_id: &str, output: String) {
            let mut sessions = self.sessions.write().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.output_queue.push(output);
            }
        }
    }

    #[async_trait]
    impl PtyExecutor for MockPty {
        async fn spawn_session(
            &self,
            name: &str,
            _command: &str,
            _args: &[&str],
            _cwd: &Path,
            _config: PtyConfig,
        ) -> Result<String> {
            let pid = {
                let mut next_pid = self.next_pid.lock().unwrap();
                let pid = *next_pid;
                *next_pid += 1;
                pid
            }; // MutexGuard 在这里 drop

            let session_id = format!("{}-{}", name, pid);

            let session = MockSession {
                pid,
                input_log: Vec::new(),
                output_queue: Vec::new(),
            };

            self.sessions.write().await.insert(session_id.clone(), session);

            Ok(session_id)
        }

        async fn has_session(&self, session_id: &str) -> Result<bool> {
            Ok(self.sessions.read().await.contains_key(session_id))
        }

        async fn send_input(&self, session_id: &str, data: &str) -> Result<()> {
            let mut sessions = self.sessions.write().await;
            let session = sessions
                .get_mut(session_id)
                .ok_or_else(|| BridgeError::SessionNotFound(session_id.to_string()))?;

            session.input_log.push(data.to_string());
            Ok(())
        }

        async fn read_output(&self, session_id: &str) -> Result<Option<String>> {
            let mut sessions = self.sessions.write().await;
            let session = sessions
                .get_mut(session_id)
                .ok_or_else(|| BridgeError::SessionNotFound(session_id.to_string()))?;

            Ok(session.output_queue.pop())
        }

        async fn kill_session(&self, session_id: &str) -> Result<()> {
            self.sessions.write().await.remove(session_id);
            Ok(())
        }

        async fn get_pid(&self, session_id: &str) -> Result<u32> {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| BridgeError::SessionNotFound(session_id.to_string()))?;

            Ok(session.pid)
        }
    }

    #[tokio::test]
    async fn test_mock_pty_spawn() {
        let pty = MockPty::new();

        let session_id = pty
            .spawn_session(
                "test",
                "bash",
                &[],
                Path::new("/tmp"),
                PtyConfig::default(),
            )
            .await
            .unwrap();

        assert!(pty.has_session(&session_id).await.unwrap());
        assert_eq!(pty.get_pid(&session_id).await.unwrap(), 1000);
    }

    #[tokio::test]
    async fn test_mock_pty_io() {
        let pty = MockPty::new();

        let session_id = pty
            .spawn_session(
                "test",
                "bash",
                &[],
                Path::new("/tmp"),
                PtyConfig::default(),
            )
            .await
            .unwrap();

        // 发送输入
        pty.send_input(&session_id, "echo hello").await.unwrap();
        pty.send_input(&session_id, "pwd").await.unwrap();

        // 验证输入日志
        let log = pty.get_input_log(&session_id).await;
        assert_eq!(log, vec!["echo hello", "pwd"]);

        // 模拟输出
        pty.queue_output(&session_id, "hello".to_string()).await;
        pty.queue_output(&session_id, "/tmp".to_string()).await;

        // 读取输出
        assert_eq!(
            pty.read_output(&session_id).await.unwrap(),
            Some("/tmp".to_string())
        );
        assert_eq!(
            pty.read_output(&session_id).await.unwrap(),
            Some("hello".to_string())
        );
        assert_eq!(pty.read_output(&session_id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_mock_pty_kill() {
        let pty = MockPty::new();

        let session_id = pty
            .spawn_session(
                "test",
                "bash",
                &[],
                Path::new("/tmp"),
                PtyConfig::default(),
            )
            .await
            .unwrap();

        assert!(pty.has_session(&session_id).await.unwrap());

        pty.kill_session(&session_id).await.unwrap();

        assert!(!pty.has_session(&session_id).await.unwrap());
    }
}
