/// Codex 日志读取器 - 事件驱动版本
///
/// 通过读取 ~/.codex/sessions 下的官方日志解析回复
/// 使用 notify-rs 实现事件驱动的文件监控，替代轮询
use crate::error::{CodexError, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use tokio::time::Instant;

/// Session ID 正则模式 (UUID)
static SESSION_ID_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .expect("Invalid regex pattern")
});

/// Codex 会话日志根目录
fn session_root() -> PathBuf {
    dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".codex")
        .join("sessions")
}

/// 读取状态（用于恢复）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadState {
    /// 日志文件路径
    pub log_path: Option<PathBuf>,
    /// 文件偏移量（字节）
    pub offset: u64,
}

impl Default for ReadState {
    fn default() -> Self {
        Self {
            log_path: None,
            offset: 0,
        }
    }
}

/// Codex 日志读取器
pub struct CodexLogReader {
    /// 会话根目录
    root: PathBuf,
    /// 优先使用的日志文件路径
    preferred_log: Option<PathBuf>,
    /// Session ID 过滤器
    session_id_filter: Option<String>,
}

impl CodexLogReader {
    /// 创建新的日志读取器
    pub fn new() -> Self {
        Self {
            root: session_root(),
            preferred_log: None,
            session_id_filter: None,
        }
    }

    /// 创建带配置的日志读取器
    pub fn with_config(
        root: Option<PathBuf>,
        log_path: Option<PathBuf>,
        session_id_filter: Option<String>,
    ) -> Self {
        Self {
            root: root.unwrap_or_else(session_root),
            preferred_log: log_path,
            session_id_filter,
        }
    }

    /// 设置优先使用的日志路径
    pub fn set_preferred_log(&mut self, log_path: Option<PathBuf>) {
        self.preferred_log = log_path;
    }

    /// 获取当前日志路径
    pub fn current_log_path(&self) -> Option<PathBuf> {
        self.latest_log()
    }

    /// 记录当前日志与偏移（用于后续恢复）
    pub fn capture_state(&self) -> ReadState {
        let log_path = self.latest_log();
        let offset = if let Some(ref path) = log_path {
            if path.exists() {
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };

        ReadState { log_path, offset }
    }

    /// 阻塞等待新的回复（使用文件监控）
    pub async fn wait_for_message(
        &mut self,
        state: ReadState,
        timeout: Duration,
    ) -> Result<(Option<String>, ReadState)> {
        self.read_since(state, timeout, true).await
    }

    /// 非阻塞读取回复
    pub async fn try_get_message(&mut self, state: ReadState) -> Result<(Option<String>, ReadState)> {
        self.read_since(state, Duration::from_secs(0), false).await
    }

    /// 直接获取最新一条回复（从文件尾部倒序扫描）
    pub fn latest_message(&self) -> Result<Option<String>> {
        let log_path = match self.latest_log() {
            Some(path) if path.exists() => path,
            _ => return Ok(None),
        };

        // 从文件尾部读取最后 256KB 或最多 50 行
        let mut file = File::open(&log_path)?;
        let file_size = file.metadata()?.len();

        let read_size = std::cmp::min(256 * 1024, file_size);
        let start_pos = file_size.saturating_sub(read_size);

        file.seek(SeekFrom::Start(start_pos))?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

        // 倒序查找最后一条有效消息
        for line in lines.iter().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(message) = Self::extract_message(&entry) {
                    return Ok(Some(message));
                }
            }
        }

        Ok(None)
    }

    /// 扫描最新的日志文件
    fn scan_latest(&self) -> Option<PathBuf> {
        if !self.root.exists() {
            return None;
        }

        let mut logs: Vec<PathBuf> = std::fs::read_dir(&self.root)
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        if logs.is_empty() {
            return None;
        }

        // 按修改时间排序
        logs.sort_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
        });

        // 按 session_id 过滤
        if let Some(ref filter) = self.session_id_filter {
            for log in logs.iter().rev() {
                if let Some(name) = log.file_name().and_then(|n| n.to_str()) {
                    if name.contains(filter) {
                        return Some(log.clone());
                    }
                }
            }
            return None;
        }

        logs.last().cloned()
    }

    /// 获取最新的日志文件路径
    fn latest_log(&self) -> Option<PathBuf> {
        // 优先使用指定的日志
        if let Some(ref preferred) = self.preferred_log {
            if preferred.exists() {
                return Some(preferred.clone());
            }
        }

        // 否则扫描最新的
        let latest = self.scan_latest();
        if latest.is_some() {
            // 更新缓存
            // Note: 这里需要 &mut self，但我们用了 interior mutability 的话会更复杂
            // 暂时保持简单，每次都扫描
        }
        latest
    }

    /// 从指定状态开始读取消息
    async fn read_since(
        &mut self,
        state: ReadState,
        timeout: Duration,
        block: bool,
    ) -> Result<(Option<String>, ReadState)> {
        let deadline = Instant::now() + timeout;
        let mut current_state = state;

        loop {
            // 确保有日志文件
            let log_path = match self.ensure_log(&current_state) {
                Ok(path) => path,
                Err(e) => {
                    if !block {
                        return Ok((None, current_state));
                    }
                    // 等待日志文件出现
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    continue;
                }
            };

            // 尝试读取新消息
            match self.read_from_log(&log_path, &mut current_state) {
                Ok(Some(message)) => {
                    // 找到消息，更新 preferred_log
                    self.preferred_log = Some(log_path.clone());
                    return Ok((Some(message), current_state));
                }
                Ok(None) => {
                    // 没有新消息
                }
                Err(e) => {
                    tracing::warn!("读取日志失败: {}", e);
                }
            }

            // 检查是否有新的日志文件
            if let Some(latest) = self.scan_latest() {
                if latest != log_path {
                    // 发现新日志文件
                    tracing::info!("切换到新日志文件: {:?}", latest);
                    current_state.log_path = Some(latest.clone());
                    current_state.offset = 0;
                    self.preferred_log = Some(latest);

                    if !block {
                        return Ok((None, current_state));
                    }
                    continue;
                }
            }

            if !block {
                return Ok((None, current_state));
            }

            // 使用文件监控等待变化
            if let Ok(has_change) =
                Self::wait_for_file_change(&log_path, deadline - Instant::now()).await
            {
                if !has_change && Instant::now() >= deadline {
                    return Ok((None, current_state));
                }
                continue;
            }

            // 超时检查
            if Instant::now() >= deadline {
                return Ok((None, current_state));
            }

            // 降级为轮询
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// 确保有可用的日志文件
    fn ensure_log(&self, state: &ReadState) -> Result<PathBuf> {
        // 1. 优先使用 preferred_log
        if let Some(ref preferred) = self.preferred_log {
            if preferred.exists() {
                return Ok(preferred.clone());
            }
        }

        // 2. 尝试使用 state 中的路径
        if let Some(ref path) = state.log_path {
            if path.exists() {
                return Ok(path.clone());
            }
        }

        // 3. 扫描最新的
        self.scan_latest()
            .ok_or_else(|| CodexError::LogNotFound("未找到 Codex session 日志".to_string()))
    }

    /// 从日志文件读取新消息
    fn read_from_log(&self, log_path: &Path, state: &mut ReadState) -> Result<Option<String>> {
        let mut file = File::open(log_path)?;
        file.seek(SeekFrom::Start(state.offset))?;

        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            state.offset += line.len() as u64 + 1; // +1 for newline

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(entry) => {
                    if let Some(message) = Self::extract_message(&entry) {
                        state.log_path = Some(log_path.to_path_buf());
                        return Ok(Some(message));
                    }
                }
                Err(e) => {
                    tracing::debug!("解析 JSON 失败: {} (行: {})", e, line);
                    continue;
                }
            }
        }

        state.log_path = Some(log_path.to_path_buf());
        Ok(None)
    }

    /// 等待文件变化（使用 notify-rs）
    async fn wait_for_file_change(path: &Path, timeout: Duration) -> Result<bool> {
        let (tx, rx) = mpsc::channel();

        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    let _ = tx.send(event);
                }
            })
            .map_err(|e| CodexError::Other(anyhow::anyhow!("创建文件监控失败: {}", e)))?;

        watcher
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|e| CodexError::Other(anyhow::anyhow!("监控文件失败: {}", e)))?;

        // 异步等待事件或超时
        let deadline = Instant::now() + timeout;
        tokio::task::spawn_blocking(move || {
            while Instant::now() < deadline {
                let remaining = deadline - Instant::now();
                match rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
                    Ok(_event) => return true,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= deadline {
                            return false;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return false,
                }
            }
            false
        })
        .await
        .map_err(|e| CodexError::Other(anyhow::anyhow!("等待文件变化失败: {}", e)))
    }

    /// 从日志条目中提取消息
    fn extract_message(entry: &serde_json::Value) -> Option<String> {
        // 检查 type 字段
        if entry.get("type")?.as_str()? != "response_item" {
            return None;
        }

        let payload = entry.get("payload")?.as_object()?;

        // 检查 payload.type
        if payload.get("type")?.as_str()? != "message" {
            return None;
        }

        // 优先从 content 数组提取
        if let Some(content) = payload.get("content").and_then(|c| c.as_array()) {
            let texts: Vec<String> = content
                .iter()
                .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("output_text"))
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .map(|s| s.to_string())
                .collect();

            if !texts.is_empty() {
                let message = texts.join("\n").trim().to_string();
                if !message.is_empty() {
                    return Some(message);
                }
            }
        }

        // 降级为 message 字段
        if let Some(message) = payload.get("message").and_then(|m| m.as_str()) {
            let message = message.trim();
            if !message.is_empty() {
                return Some(message.to_string());
            }
        }

        None
    }

    /// 从日志路径提取 session ID
    pub fn extract_session_id(log_path: &Path) -> Option<String> {
        // 1. 从文件名提取
        if let Some(name) = log_path.file_stem().and_then(|s| s.to_str()) {
            if let Some(caps) = SESSION_ID_PATTERN.captures(name) {
                return Some(caps.get(0)?.as_str().to_string());
            }
        }

        // 2. 从完整文件名提取
        if let Some(name) = log_path.file_name().and_then(|s| s.to_str()) {
            if let Some(caps) = SESSION_ID_PATTERN.captures(name) {
                return Some(caps.get(0)?.as_str().to_string());
            }
        }

        // 3. 从文件第一行提取
        if let Ok(file) = File::open(log_path) {
            let mut reader = BufReader::new(file);
            let mut first_line = String::new();
            if reader.read_line(&mut first_line).is_ok() {
                // 先尝试正则直接匹配
                if let Some(caps) = SESSION_ID_PATTERN.captures(&first_line) {
                    return Some(caps.get(0)?.as_str().to_string());
                }

                // 再尝试解析 JSON
                if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&first_line) {
                    // 尝试多个可能的字段
                    let candidates = vec![
                        entry.get("session_id"),
                        entry.get("payload").and_then(|p| p.get("id")),
                        entry
                            .get("payload")
                            .and_then(|p| p.get("session"))
                            .and_then(|s| s.get("id")),
                    ];

                    for candidate in candidates {
                        if let Some(value) = candidate.and_then(|v| v.as_str()) {
                            if let Some(caps) = SESSION_ID_PATTERN.captures(value) {
                                return Some(caps.get(0)?.as_str().to_string());
                            }
                        }
                    }
                }
            }
        }

        None
    }
}

impl Default for CodexLogReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_extract_message() {
        let json = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Hello, world!"
                    }
                ]
            }
        });

        let message = CodexLogReader::extract_message(&json);
        assert_eq!(message, Some("Hello, world!".to_string()));
    }

    #[test]
    fn test_extract_message_fallback() {
        let json = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "message": "Fallback message"
            }
        });

        let message = CodexLogReader::extract_message(&json);
        assert_eq!(message, Some("Fallback message".to_string()));
    }

    #[test]
    fn test_extract_session_id_from_filename() {
        let path = PathBuf::from("/tmp/abc-12345678-1234-1234-1234-123456789abc.jsonl");
        let session_id = CodexLogReader::extract_session_id(&path);
        assert_eq!(
            session_id,
            Some("12345678-1234-1234-1234-123456789abc".to_string())
        );
    }

    #[tokio::test]
    async fn test_capture_state() {
        let temp = TempDir::new().unwrap();
        let log_file = temp.path().join("test.jsonl");

        // 写入一些数据
        let mut file = File::create(&log_file).unwrap();
        writeln!(file, "line 1").unwrap();
        writeln!(file, "line 2").unwrap();

        let reader = CodexLogReader::with_config(Some(temp.path().to_path_buf()), Some(log_file.clone()), None);
        let state = reader.capture_state();

        assert_eq!(state.log_path, Some(log_file));
        assert!(state.offset > 0);
    }

    #[tokio::test]
    async fn test_read_from_log() {
        let temp = TempDir::new().unwrap();
        let log_file = temp.path().join("test.jsonl");

        // 写入测试日志
        let mut file = File::create(&log_file).unwrap();
        writeln!(
            file,
            r#"{{"type":"response_item","payload":{{"type":"message","message":"Test reply"}}}}"#
        )
        .unwrap();

        let reader = CodexLogReader::with_config(Some(temp.path().to_path_buf()), Some(log_file.clone()), None);
        let mut state = ReadState {
            log_path: Some(log_file.clone()),
            offset: 0,
        };

        let result = reader.read_from_log(&log_file, &mut state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("Test reply".to_string()));
        assert!(state.offset > 0);
    }

    #[test]
    fn test_latest_message() {
        let temp = TempDir::new().unwrap();
        let log_file = temp.path().join("test.jsonl");

        // 写入多条日志
        let mut file = File::create(&log_file).unwrap();
        writeln!(
            file,
            r#"{{"type":"other","payload":{{}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"response_item","payload":{{"type":"message","message":"First"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"response_item","payload":{{"type":"message","message":"Latest"}}}}"#
        )
        .unwrap();

        let reader = CodexLogReader::with_config(Some(temp.path().to_path_buf()), Some(log_file), None);
        let message = reader.latest_message().unwrap();
        assert_eq!(message, Some("Latest".to_string()));
    }
}
