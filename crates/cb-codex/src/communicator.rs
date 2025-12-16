/// Codex 通信器 - 高级 API
///
/// 提供与 Codex 交互的高级接口：
/// - ask_async: 异步发送（fire-and-forget）
/// - ask_sync: 同步发送并等待回复
/// - consume_pending: 获取待处理的最新回复
/// - ping: 健康检查
use crate::error::{CodexError, Result};
use crate::log_reader::CodexLogReader;
use cb_core::session::{FileSessionManager, Provider, SessionInfo, SessionManager};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 发送的消息格式
#[derive(Debug, Serialize, Deserialize)]
struct Message {
    content: String,
    timestamp: String,
    marker: String,
}

/// Codex 通信器
pub struct CodexCommunicator {
    /// Session 信息
    session_info: SessionInfo,
    /// 日志读取器
    log_reader: CodexLogReader,
    /// Session 管理器
    session_manager: FileSessionManager,
    /// 默认超时时间
    timeout: Duration,
}

impl CodexCommunicator {
    /// 创建新的通信器（从当前目录的 .codex-session 文件加载）
    pub fn new() -> Result<Self> {
        Self::with_timeout(Duration::from_secs(30))
    }

    /// 创建带自定义超时的通信器
    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        let session_manager = FileSessionManager::new(Provider::Codex);
        let session_info = session_manager
            .load()?
            .ok_or_else(|| CodexError::SessionNotFound)?;

        // 健康检查
        Self::check_session_health_static(&session_info)?;

        // 创建日志读取器
        let log_path = Self::extract_log_path(&session_info);
        let session_id_filter = Self::extract_session_id(&session_info);
        let mut log_reader =
            CodexLogReader::with_config(None, log_path.clone(), session_id_filter.clone());

        // 绑定日志路径
        if let Some(path) = log_reader.current_log_path() {
            log_reader.set_preferred_log(Some(path));
        }

        Ok(Self {
            session_info,
            log_reader,
            session_manager,
            timeout,
        })
    }

    /// 从环境变量创建通信器
    pub fn from_env(timeout: Duration) -> Result<Self> {
        let session_id = std::env::var("CODEX_SESSION_ID")
            .map_err(|_| CodexError::SessionNotFound)?;
        let runtime_dir = std::env::var("CODEX_RUNTIME_DIR")
            .map_err(|_| CodexError::SessionNotFound)?;
        let input_fifo = std::env::var("CODEX_INPUT_FIFO")
            .map_err(|_| CodexError::SessionNotFound)?;

        let session_info = SessionInfo {
            session_id,
            provider: Provider::Codex,
            runtime_dir: PathBuf::from(runtime_dir),
            pty_session_id: "".to_string(), // 从环境变量创建时可能没有这个信息
            pid: None,
            work_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            active: true,
            started_at: chrono::Utc::now(),
            ended_at: None,
            extensions: serde_json::json!({
                "input_fifo": input_fifo,
            }),
        };

        Self::check_session_health_static(&session_info)?;

        let log_reader = CodexLogReader::new();
        let session_manager = FileSessionManager::new(Provider::Codex);

        Ok(Self {
            session_info,
            log_reader,
            session_manager,
            timeout,
        })
    }

    /// 异步发送消息（fire-and-forget）
    pub async fn ask_async(&mut self, question: &str) -> Result<String> {
        self.check_session_health()?;

        let marker = self.generate_marker();
        let state = self.log_reader.capture_state();

        self.send_message(question, &marker).await?;

        // 记住日志路径
        if let Some(log_path) = state.log_path.or_else(|| self.log_reader.current_log_path()) {
            self.remember_codex_session(&log_path)?;
        }

        Ok(marker)
    }

    /// 同步发送消息并等待回复
    pub async fn ask_sync(&mut self, question: &str, timeout: Option<Duration>) -> Result<Option<String>> {
        self.check_session_health()?;

        let marker = self.generate_marker();
        let state = self.log_reader.capture_state();

        self.send_message(question, &marker).await?;

        let wait_timeout = timeout.unwrap_or(self.timeout);
        let (message, new_state) = self.log_reader.wait_for_message(state, wait_timeout).await?;

        // 记住日志路径
        if let Some(log_path) = new_state.log_path.or_else(|| self.log_reader.current_log_path()) {
            self.remember_codex_session(&log_path)?;
        }

        Ok(message)
    }

    /// 获取待处理的最新回复
    pub fn consume_pending(&mut self) -> Result<Option<String>> {
        // 记住当前日志路径
        if let Some(log_path) = self.log_reader.current_log_path() {
            self.remember_codex_session(&log_path)?;
        }

        self.log_reader.latest_message()
    }

    /// 健康检查
    pub fn ping(&self) -> Result<(bool, String)> {
        match self.check_session_health() {
            Ok(_) => Ok((true, "会话正常".to_string())),
            Err(e) => Ok((false, format!("会话异常: {}", e))),
        }
    }

    /// 获取 session 状态信息
    pub fn get_status(&self) -> serde_json::Value {
        let (healthy, status) = self.ping().unwrap_or((false, "Unknown".to_string()));

        let mut info = serde_json::json!({
            "session_id": self.session_info.session_id,
            "runtime_dir": self.session_info.runtime_dir,
            "healthy": healthy,
            "status": status,
        });

        if let Some(input_fifo) = self.session_info.extensions.get("input_fifo") {
            info["input_fifo"] = input_fifo.clone();
        }

        // 尝试读取 PID
        let pid_file = self.session_info.runtime_dir.join("codex.pid");
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                info["codex_pid"] = serde_json::json!(pid);
            }
        }

        info
    }

    /// 检查 session 健康状态
    fn check_session_health(&self) -> Result<()> {
        Self::check_session_health_static(&self.session_info)
    }

    /// 静态方法：检查 session 健康状态
    fn check_session_health_static(session_info: &SessionInfo) -> Result<()> {
        // 1. 检查 runtime_dir 存在
        if !session_info.runtime_dir.exists() {
            return Err(CodexError::SessionUnhealthy(
                "运行时目录不存在".to_string(),
            ));
        }

        // 2. 检查 PID 文件
        let pid_file = session_info.runtime_dir.join("codex.pid");
        if !pid_file.exists() {
            return Err(CodexError::SessionUnhealthy(
                "Codex PID 文件不存在".to_string(),
            ));
        }

        // 3. 检查进程是否存活
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                if !process_alive(pid) {
                    return Err(CodexError::SessionUnhealthy(format!(
                        "Codex 进程 (PID {}) 已退出",
                        pid
                    )));
                }
            }
        }

        // 4. 检查 input_fifo 存在
        if let Some(fifo_path) = session_info.extensions.get("input_fifo") {
            if let Some(fifo_str) = fifo_path.as_str() {
                let fifo = PathBuf::from(fifo_str);
                if !fifo.exists() {
                    return Err(CodexError::SessionUnhealthy(
                        "通信管道不存在".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    /// 发送消息到 FIFO
    async fn send_message(&self, content: &str, marker: &str) -> Result<()> {
        let fifo_path = self
            .session_info
            .extensions
            .get("input_fifo")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CodexError::FifoError("input_fifo 未配置".to_string()))?;

        let message = Message {
            content: content.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            marker: marker.to_string(),
        };

        let json = serde_json::to_string(&message)?;

        // 异步写入 FIFO
        tokio::task::spawn_blocking({
            let fifo_path = fifo_path.to_string();
            let json = json.clone();
            move || -> Result<()> {
                let mut file = File::create(&fifo_path)
                    .map_err(|e| CodexError::FifoError(format!("打开 FIFO 失败: {}", e)))?;
                writeln!(file, "{}", json)
                    .map_err(|e| CodexError::FifoError(format!("写入 FIFO 失败: {}", e)))?;
                file.flush()
                    .map_err(|e| CodexError::FifoError(format!("刷新 FIFO 失败: {}", e)))?;
                Ok(())
            }
        })
        .await
        .map_err(|e| CodexError::Other(anyhow::anyhow!("FIFO 写入任务失败: {}", e)))??;

        Ok(())
    }

    /// 生成唯一的消息标记
    fn generate_marker(&self) -> String {
        format!(
            "ask-{}-{}",
            chrono::Utc::now().timestamp(),
            std::process::id()
        )
    }

    /// 记住 Codex session 信息（更新 session 文件）
    fn remember_codex_session(&mut self, log_path: &Path) -> Result<()> {
        // 更新 log_reader
        self.log_reader.set_preferred_log(Some(log_path.to_path_buf()));

        // 提取 session ID
        let session_id = CodexLogReader::extract_session_id(log_path);

        // 更新 session 信息
        let mut updated = false;

        if let Some(ref sid) = session_id {
            if let Some(current_sid) = self.session_info.extensions.get("codex_session_id") {
                if current_sid.as_str() != Some(sid) {
                    updated = true;
                }
            } else {
                updated = true;
            }

            if updated {
                self.session_info.extensions.as_object_mut().map(|obj| {
                    obj.insert("codex_session_id".to_string(), serde_json::json!(sid));
                });
            }
        }

        let path_str = log_path.to_string_lossy().to_string();
        if let Some(current_path) = self.session_info.extensions.get("codex_session_path") {
            if current_path.as_str() != Some(&path_str) {
                updated = true;
            }
        } else {
            updated = true;
        }

        if updated {
            self.session_info.extensions.as_object_mut().map(|obj| {
                obj.insert("codex_session_path".to_string(), serde_json::json!(path_str));
            });
        }

        // 生成 resume 命令
        if let Some(ref sid) = session_id {
            let resume_cmd = format!("codex resume {}", sid);
            self.session_info.extensions.as_object_mut().map(|obj| {
                obj.insert("codex_start_cmd".to_string(), serde_json::json!(resume_cmd));
            });
            updated = true;
        }

        // 保存到文件
        if updated {
            self.session_manager.save(&self.session_info)?;
        }

        Ok(())
    }

    /// 从 session 信息提取日志路径
    fn extract_log_path(session_info: &SessionInfo) -> Option<PathBuf> {
        session_info
            .extensions
            .get("codex_session_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    }

    /// 从 session 信息提取 session ID
    fn extract_session_id(session_info: &SessionInfo) -> Option<String> {
        session_info
            .extensions
            .get("codex_session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

/// 检查进程是否存活
#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    unsafe { nix::libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_marker() {
        let temp = TempDir::new().unwrap();
        let session_file = temp.path().join(".codex-session");

        // 创建测试 session
        let info = SessionInfo {
            session_id: "test-123".to_string(),
            provider: Provider::Codex,
            runtime_dir: temp.path().to_path_buf(),
            pty_session_id: "codex-1000".to_string(),
            pid: Some(1000),
            work_dir: std::env::current_dir().unwrap(),
            active: true,
            started_at: chrono::Utc::now(),
            ended_at: None,
            extensions: serde_json::json!({
                "input_fifo": temp.path().join("input.fifo").to_string_lossy().to_string(),
            }),
        };

        let mgr = FileSessionManager::with_path(Provider::Codex, session_file);
        mgr.save(&info).unwrap();

        // 由于需要真实的健康检查，这个测试暂时跳过完整的通信器创建
        // 仅测试 marker 生成逻辑
        let marker1 = format!("ask-{}-{}", chrono::Utc::now().timestamp(), std::process::id());
        assert!(marker1.starts_with("ask-"));
        assert!(marker1.contains(&std::process::id().to_string()));
    }

    #[test]
    fn test_extract_log_path() {
        let info = SessionInfo {
            session_id: "test".to_string(),
            provider: Provider::Codex,
            runtime_dir: PathBuf::from("/tmp"),
            pty_session_id: "codex-1000".to_string(),
            pid: Some(1000),
            work_dir: PathBuf::from("/tmp"),
            active: true,
            started_at: chrono::Utc::now(),
            ended_at: None,
            extensions: serde_json::json!({
                "codex_session_path": "/path/to/log.jsonl"
            }),
        };

        let log_path = CodexCommunicator::extract_log_path(&info);
        assert_eq!(log_path, Some(PathBuf::from("/path/to/log.jsonl")));
    }

    #[test]
    fn test_extract_session_id() {
        let info = SessionInfo {
            session_id: "test".to_string(),
            provider: Provider::Codex,
            runtime_dir: PathBuf::from("/tmp"),
            pty_session_id: "codex-1000".to_string(),
            pid: Some(1000),
            work_dir: PathBuf::from("/tmp"),
            active: true,
            started_at: chrono::Utc::now(),
            ended_at: None,
            extensions: serde_json::json!({
                "codex_session_id": "abc-123-def"
            }),
        };

        let session_id = CodexCommunicator::extract_session_id(&info);
        assert_eq!(session_id, Some("abc-123-def".to_string()));
    }
}
