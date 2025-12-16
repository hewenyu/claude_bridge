/// Session 守护进程架构
///
/// 提供类似 tmux 的 attach/detach 功能，但是纯 Rust 实现
use crate::error::{BridgeError, Result};
use crate::pty::{PtyExecutor, SystemPty};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};

/// 守护进程管理的会话
struct ManagedSession {
    pty_session_id: String,
    work_dir: PathBuf,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// 守护进程
pub struct SessionDaemon {
    sessions: Arc<RwLock<HashMap<String, ManagedSession>>>,
    pty: Arc<SystemPty>,
    socket_path: PathBuf,
}

/// 客户端请求
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonRequest {
    /// 创建新 session
    Create {
        name: String,
        command: String,
        args: Vec<String>,
        cwd: PathBuf,
    },
    /// Attach 到 session
    Attach { session_id: String },
    /// Detach（客户端关闭连接即可）
    Detach,
    /// 列出所有 sessions
    List,
    /// 终止 session
    Kill { session_id: String },
}

/// 守护进程响应
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    /// 创建成功
    Created { session_id: String, pid: u32 },
    /// Attach 成功（之后开始双向数据转发）
    Attached,
    /// Session 列表
    SessionList(Vec<SessionListItem>),
    /// 操作成功
    Ok,
    /// 错误
    Error(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionListItem {
    pub session_id: String,
    pub work_dir: PathBuf,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl SessionDaemon {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pty: Arc::new(SystemPty::new()),
            socket_path,
        }
    }

    /// 启动守护进程
    pub async fn start(&self) -> Result<()> {
        // 删除旧的 socket 文件
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|e| BridgeError::Other(anyhow::anyhow!("Failed to bind socket: {}", e)))?;

        tracing::info!("Daemon listening on {:?}", self.socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let sessions = self.sessions.clone();
                    let pty = self.pty.clone();

                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_client(stream, sessions, pty).await {
                            tracing::error!("Client handler error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }

    async fn handle_client(
        mut stream: UnixStream,
        sessions: Arc<RwLock<HashMap<String, ManagedSession>>>,
        pty: Arc<SystemPty>,
    ) -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // 读取请求
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request: DaemonRequest = serde_json::from_slice(&buf[..n])
            .map_err(|e| BridgeError::Other(anyhow::anyhow!("Invalid request: {}", e)))?;

        match request {
            DaemonRequest::Create { name, command, args, cwd } => {
                let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let session_id = pty
                    .spawn_session(
                        &name,
                        &command,
                        &args_refs,
                        &cwd,
                        Default::default(),
                    )
                    .await?;

                let pid = pty.get_pid(&session_id).await?;

                let managed = ManagedSession {
                    pty_session_id: session_id.clone(),
                    work_dir: cwd,
                    created_at: chrono::Utc::now(),
                };

                sessions.write().await.insert(session_id.clone(), managed);

                let response = DaemonResponse::Created { session_id, pid };
                let response_json = serde_json::to_vec(&response)?;
                stream.write_all(&response_json).await?;
            }

            DaemonRequest::Attach { session_id } => {
                // 验证 session 存在
                if !sessions.read().await.contains_key(&session_id) {
                    let response = DaemonResponse::Error("Session not found".to_string());
                    let response_json = serde_json::to_vec(&response)?;
                    stream.write_all(&response_json).await?;
                    return Ok(());
                }

                // 发送 Attached 响应
                let response = DaemonResponse::Attached;
                let response_json = serde_json::to_vec(&response)?;
                stream.write_all(&response_json).await?;

                // TODO: 实现双向数据转发
                // stdin -> PTY input
                // PTY output -> stdout
                // 这需要更复杂的异步 I/O 处理
            }

            DaemonRequest::List => {
                let sessions_guard = sessions.read().await;
                let list: Vec<SessionListItem> = sessions_guard
                    .iter()
                    .map(|(id, session)| SessionListItem {
                        session_id: id.clone(),
                        work_dir: session.work_dir.clone(),
                        created_at: session.created_at,
                    })
                    .collect();

                let response = DaemonResponse::SessionList(list);
                let response_json = serde_json::to_vec(&response)?;
                stream.write_all(&response_json).await?;
            }

            DaemonRequest::Kill { session_id } => {
                if let Some(_) = sessions.write().await.remove(&session_id) {
                    pty.kill_session(&session_id).await?;
                    let response = DaemonResponse::Ok;
                    let response_json = serde_json::to_vec(&response)?;
                    stream.write_all(&response_json).await?;
                } else {
                    let response = DaemonResponse::Error("Session not found".to_string());
                    let response_json = serde_json::to_vec(&response)?;
                    stream.write_all(&response_json).await?;
                }
            }

            DaemonRequest::Detach => {
                // 客户端关闭连接即可
                let response = DaemonResponse::Ok;
                let response_json = serde_json::to_vec(&response)?;
                stream.write_all(&response_json).await?;
            }
        }

        Ok(())
    }
}

/// 客户端连接到守护进程
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub async fn send_request(&self, request: DaemonRequest) -> Result<DaemonResponse> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = UnixStream::connect(&self.socket_path).await?;

        let request_json = serde_json::to_vec(&request)?;
        stream.write_all(&request_json).await?;

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let response: DaemonResponse = serde_json::from_slice(&buf[..n])?;

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_daemon_basic() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("daemon.sock");

        let daemon = SessionDaemon::new(socket.clone());

        // 启动守护进程（后台）
        tokio::spawn(async move {
            daemon.start().await.unwrap();
        });

        // 等待守护进程启动
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // 连接客户端
        let client = DaemonClient::new(socket);

        // 列出 sessions（应该为空）
        let response = client.send_request(DaemonRequest::List).await.unwrap();
        match response {
            DaemonResponse::SessionList(list) => {
                assert_eq!(list.len(), 0);
            }
            _ => panic!("Expected SessionList"),
        }
    }
}
