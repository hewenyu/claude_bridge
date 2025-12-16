use crate::error::{BridgeError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tempfile::NamedTempFile;
use std::io::Write;

/// Provider 类型
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Gemini,
}

impl Provider {
    pub fn session_filename(&self) -> &'static str {
        match self {
            Provider::Codex => ".codex-session",
            Provider::Gemini => ".gemini-session",
        }
    }
}

/// Session 信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub provider: Provider,
    pub runtime_dir: PathBuf,
    pub tmux_session: String,
    pub work_dir: PathBuf,
    pub active: bool,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,

    /// Provider-specific extensions (stored as JSON)
    #[serde(flatten)]
    pub extensions: serde_json::Value,
}

/// Session health status
#[derive(Debug)]
pub struct HealthStatus {
    pub healthy: bool,
    pub message: String,
    pub details: serde_json::Value,
}

/// Session 管理 trait
pub trait SessionManager: Send + Sync {
    fn load(&self) -> Result<Option<SessionInfo>>;
    fn save(&self, info: &SessionInfo) -> Result<()>;
    fn mark_inactive(&self) -> Result<()>;
    fn check_health(&self) -> Result<HealthStatus>;
}

/// 基于文件的 Session 管理器
pub struct FileSessionManager {
    session_file: PathBuf,
    provider: Provider,
}

impl FileSessionManager {
    pub fn new(provider: Provider) -> Self {
        let filename = provider.session_filename();
        Self {
            session_file: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(filename),
            provider,
        }
    }

    pub fn with_path(provider: Provider, path: PathBuf) -> Self {
        Self {
            session_file: path,
            provider,
        }
    }

    /// 原子性写入 JSON 文件
    fn atomic_write_json<T: Serialize>(&self, data: &T) -> Result<()> {
        let json = serde_json::to_string_pretty(data)?;

        // 在同一目录下创建临时文件
        let dir = self.session_file
            .parent()
            .ok_or_else(|| BridgeError::Other(anyhow::anyhow!("Invalid session file path")))?;

        let mut temp = NamedTempFile::new_in(dir)?;
        temp.write_all(json.as_bytes())?;
        temp.flush()?;

        // 设置权限为 0644
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = temp.as_file().metadata()?;
            let mut perms = metadata.permissions();
            perms.set_mode(0o644);
            temp.as_file().set_permissions(perms)?;
        }

        // 原子性重命名
        temp.persist(&self.session_file)
            .map_err(|e| BridgeError::Io(e.error))?;

        Ok(())
    }
}

impl SessionManager for FileSessionManager {
    fn load(&self) -> Result<Option<SessionInfo>> {
        if !self.session_file.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&self.session_file)?;
        let info: SessionInfo = serde_json::from_str(&content)
            .map_err(|e| BridgeError::InvalidSessionFormat(e.to_string()))?;

        // 验证 provider 匹配
        if info.provider != self.provider {
            return Err(BridgeError::InvalidSessionFormat(format!(
                "Provider mismatch: expected {:?}, got {:?}",
                self.provider, info.provider
            )));
        }

        // 如果不活跃，返回 None
        if !info.active {
            return Ok(None);
        }

        // 验证 runtime_dir 存在
        if !info.runtime_dir.exists() {
            return Ok(None);
        }

        Ok(Some(info))
    }

    fn save(&self, info: &SessionInfo) -> Result<()> {
        self.atomic_write_json(info)
    }

    fn mark_inactive(&self) -> Result<()> {
        let mut info = self.load()?
            .ok_or_else(|| BridgeError::SessionNotFound(self.provider.session_filename().to_string()))?;

        info.active = false;
        info.ended_at = Some(Utc::now());

        self.save(&info)
    }

    fn check_health(&self) -> Result<HealthStatus> {
        let info = self.load()?
            .ok_or_else(|| BridgeError::SessionNotFound(self.provider.session_filename().to_string()))?;

        // 检查 runtime_dir 是否存在
        if !info.runtime_dir.exists() {
            return Ok(HealthStatus {
                healthy: false,
                message: "Runtime directory does not exist".to_string(),
                details: serde_json::json!({
                    "runtime_dir": info.runtime_dir,
                }),
            });
        }

        // 检查 PID 文件（如果是 Codex）
        if info.provider == Provider::Codex {
            let pid_file = info.runtime_dir.join("codex.pid");
            if !pid_file.exists() {
                return Ok(HealthStatus {
                    healthy: false,
                    message: "Codex PID file not found".to_string(),
                    details: serde_json::json!({
                        "pid_file": pid_file,
                    }),
                });
            }

            // 读取 PID 并检查进程是否存活
            if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = pid_str.trim().parse::<i32>() {
                    if !process_alive(pid) {
                        return Ok(HealthStatus {
                            healthy: false,
                            message: format!("Codex process (PID {}) not running", pid),
                            details: serde_json::json!({
                                "pid": pid,
                            }),
                        });
                    }
                }
            }
        }

        Ok(HealthStatus {
            healthy: true,
            message: "Session healthy".to_string(),
            details: serde_json::json!({
                "session_id": info.session_id,
                "runtime_dir": info.runtime_dir,
            }),
        })
    }
}

/// 检查进程是否存活
#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // Signal 0 不会发送信号，只是检查进程是否存在
    // 使用 nix 提供的 libc
    unsafe {
        nix::libc::kill(pid, 0) == 0
    }
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    // Windows 实现（暂不支持）
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_session_save_load() {
        let temp = TempDir::new().unwrap();
        let session_file = temp.path().join(".codex-session");

        let mgr = FileSessionManager::with_path(Provider::Codex, session_file.clone());

        let info = SessionInfo {
            session_id: "test-123".to_string(),
            provider: Provider::Codex,
            runtime_dir: temp.path().to_path_buf(),
            tmux_session: "codex-123".to_string(),
            work_dir: std::env::current_dir().unwrap(),
            active: true,
            started_at: Utc::now(),
            ended_at: None,
            extensions: serde_json::json!({
                "input_fifo": "/tmp/test.fifo"
            }),
        };

        mgr.save(&info).unwrap();
        let loaded = mgr.load().unwrap().unwrap();

        assert_eq!(loaded.session_id, "test-123");
        assert_eq!(loaded.active, true);
        assert_eq!(loaded.provider, Provider::Codex);
    }

    #[test]
    fn test_mark_inactive() {
        let temp = TempDir::new().unwrap();
        let session_file = temp.path().join(".codex-session");

        let mgr = FileSessionManager::with_path(Provider::Codex, session_file);

        let info = SessionInfo {
            session_id: "test-456".to_string(),
            provider: Provider::Codex,
            runtime_dir: temp.path().to_path_buf(),
            tmux_session: "codex-456".to_string(),
            work_dir: std::env::current_dir().unwrap(),
            active: true,
            started_at: Utc::now(),
            ended_at: None,
            extensions: serde_json::Value::Object(serde_json::Map::new()),
        };

        mgr.save(&info).unwrap();
        mgr.mark_inactive().unwrap();

        // 加载后应该是 None（因为 active=false）
        let loaded = mgr.load().unwrap();
        assert!(loaded.is_none());
    }
}
