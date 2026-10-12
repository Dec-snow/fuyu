//! MCP 客户端。
//!
//! ## 做到哪一步，以及为什么
//!
//! 只支持 **stdio 传输**：MCP 服务端绝大多数是本地进程
//! （`npx -y @modelcontextprotocol/server-filesystem /path`、`uvx mcp-server-git`）。
//! HTTP/SSE 传输没做 —— 那是**明确的取舍**，写在 README 的「未实现」列表里，
//! 而不是假装支持。
//!
//! ## 三条设计上的取舍
//!
//! 1. **进程常驻**：一个服务器只启动一次，之后复用。MCP 握手要往返两次
//!    （`initialize` + `initialized`），每次调用都重启进程既慢又会漏掉服务端的
//!    会话状态。
//! 2. **工具清单缓存起来**：`tools/list` 是异步的，而构造工具清单（`tool_definitions`）
//!    是同步的 —— 所以发现结果存进 `McpManager`，用的时候同步读。刷新由界面上的
//!    「重新扫描」按钮触发。这不是偷懒：聊天主路径上不能插入「启动若干子进程」
//!    这种可能卡住好几秒的操作。
//! 3. **每个服务器一把锁**：同一个服务器的调用串行（stdio 是一条流，并发写会串台），
//!    不同服务器互不阻塞。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::diagnostics::LogBuffer;
use crate::errors::{code, AppError, AppResult};
use crate::storage::config::McpServerConfig;

use super::protocol::{
    self, build_notification, build_request, call_params, initialize_params, parse_message,
    parse_server_info, parse_tools_list, tool_call_text, McpTool, RpcMessage,
};

/// 单次请求的超时。
///
/// 30 秒是权衡的结果：MCP 工具里可能有慢操作（读大文件、跑一次 git log），
/// 但「服务端卡死」也必须能在有限时间内被判出来 —— 否则聊天会一直停在
/// 「正在执行工具」，用户只能强杀应用。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 启动 + 握手的超时（比单次请求短：连不上要快点报错）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// 与一个 MCP 服务端的一条会话。
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    /// 写一条请求并等它的应答。期间收到的通知跳过、别人的应答也跳过。
    async fn request(&mut self, method: &str, params: serde_json::Value) -> AppResult<RpcMessage> {
        self.next_id += 1;
        let id = self.next_id;
        let line = build_request(id, method, params);
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| AppError::new(code::SERVER, format!("写入 MCP 服务端失败：{e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| AppError::new(code::SERVER, format!("写入 MCP 服务端失败：{e}")))?;

        let mut buffer = String::new();
        loop {
            buffer.clear();
            let read = tokio::time::timeout(REQUEST_TIMEOUT, self.stdout.read_line(&mut buffer))
                .await
                .map_err(|_| {
                    AppError::new(
                        code::TIMEOUT,
                        format!("MCP 服务端超过 {} 秒没有响应", REQUEST_TIMEOUT.as_secs()),
                    )
                })?
                .map_err(|e| AppError::new(code::SERVER, format!("读取 MCP 服务端失败：{e}")))?;
            // 0 表示对端关了流
            if read == 0 {
                return Err(AppError::new(
                    code::SERVER,
                    "MCP 服务端关掉了连接（可能是它自己退出了）",
                ));
            }
            match parse_message(&buffer) {
                Some(RpcMessage::Response {
                    id: response_id,
                    result,
                    error,
                }) if response_id == id => return Ok(RpcMessage::Response { id, result, error }),
                // 通知与别人的应答：继续读
                _ => continue,
            }
        }
    }
}

/// MCP 服务器管理器。
pub struct McpManager {
    /// 每个服务器一条常驻会话（`None` 表示启动失败过，下次可以重试）。
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
    /// 上次发现的工具（同步可读）。
    tools: Mutex<HashMap<String, Vec<McpTool>>>,
    logs: LogBuffer,
}

impl McpManager {
    pub fn new(logs: LogBuffer) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            tools: Mutex::new(HashMap::new()),
            logs,
        }
    }

    /// 当前缓存的全部 MCP 工具（同步，供构造工具清单用）。
    pub async fn cached_tools(&self) -> Vec<McpTool> {
        let tools = self.tools.lock().await;
        tools.values().flatten().cloned().collect()
    }

    /// 按名字找缓存的工具（调用时用）。
    pub async fn find_tool(&self, qualified_name: &str) -> Option<McpTool> {
        let tools = self.tools.lock().await;
        tools
            .values()
            .flatten()
            .find(|t| t.name == qualified_name)
            .cloned()
    }

    /// 刷新所有启用服务器的工具清单。
    pub async fn refresh(&self, servers: &[McpServerConfig]) -> Vec<protocol::McpServerStatus> {
        let mut statuses = Vec::new();
        let mut discovered: HashMap<String, Vec<McpTool>> = HashMap::new();

        for server in servers.iter().filter(|s| s.enabled) {
            match self.connect_and_list(server).await {
                Ok((server_name, server_version, tools)) => {
                    discovered.insert(server.id.clone(), tools.clone());
                    statuses.push(protocol::McpServerStatus {
                        id: server.id.clone(),
                        name: server.name.clone(),
                        connected: true,
                        server_name,
                        server_version,
                        tools,
                        error: None,
                    });
                }
                Err(e) => {
                    self.logs
                        .record("mcp", format!("{} 连接失败：{e}", server.name));
                    statuses.push(protocol::McpServerStatus {
                        id: server.id.clone(),
                        name: server.name.clone(),
                        connected: false,
                        server_name: None,
                        server_version: None,
                        tools: Vec::new(),
                        error: Some(e.message.clone()),
                    });
                }
            }
        }

        // 被关掉或删掉的服务器的工具必须一起清掉，否则它们会继续出现在
        // 模型可见的工具清单里，而调用时又找不到会话。
        *self.tools.lock().await = discovered;
        statuses
    }

    /// 启动（或复用）一个服务器，握手并列出工具。
    async fn connect_and_list(
        &self,
        server: &McpServerConfig,
    ) -> AppResult<(Option<String>, Option<String>, Vec<McpTool>)> {
        let session = self.session(server).await?;
        let mut guard = session.lock().await;

        let handshake = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            guard.request("initialize", initialize_params()),
        )
        .await
        .map_err(|_| {
            AppError::new(
                code::TIMEOUT,
                format!("MCP 服务端 {} 秒内没完成握手", HANDSHAKE_TIMEOUT.as_secs()),
            )
        })??;

        let result = match handshake {
            RpcMessage::Response { result, error, .. } => {
                if let Some(error) = error {
                    return Err(AppError::new(
                        code::SERVER,
                        format!("MCP 握手失败：{error}"),
                    ));
                }
                result.unwrap_or_else(|| serde_json::json!({}))
            }
            _ => return Err(AppError::new(code::SERVER, "MCP 握手没有拿到应答")),
        };
        let (server_name, server_version) = parse_server_info(&result);

        // 握手之后必须发这条通知（协议要求），否则有的服务端会拒绝后续请求
        let line = build_notification("notifications/initialized", serde_json::json!({}));
        let _ = guard.stdin.write_all(line.as_bytes()).await;
        let _ = guard.stdin.flush().await;

        let listed = guard.request("tools/list", serde_json::json!({})).await?;
        let result = match listed {
            RpcMessage::Response { result, error, .. } => {
                if let Some(error) = error {
                    return Err(AppError::new(
                        code::SERVER,
                        format!("列出工具失败：{error}"),
                    ));
                }
                result.unwrap_or_else(|| serde_json::json!({}))
            }
            _ => return Err(AppError::new(code::SERVER, "列出工具没有拿到应答")),
        };

        Ok((
            server_name,
            server_version,
            parse_tools_list(&result, &server.id),
        ))
    }

    /// 取（必要时创建）一个服务器的会话。
    async fn session(&self, server: &McpServerConfig) -> AppResult<Arc<Mutex<Session>>> {
        let mut sessions = self.sessions.lock().await;
        if let Some(existing) = sessions.get(&server.id) {
            return Ok(existing.clone());
        }

        let mut command = tokio::process::Command::new(&server.command);
        command
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &server.env {
            command.env(key, value);
        }
        // Windows 上不弹一个黑框出来
        #[cfg(windows)]
        {
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }

        let mut child = command.spawn().map_err(|e| {
            AppError::new(
                code::SERVER,
                format!("启动 MCP 服务端失败（{}）：{e}", server.command),
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::new(code::SERVER, "拿不到 MCP 服务端的输入流"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::new(code::SERVER, "拿不到 MCP 服务端的输出流"))?;

        // stderr 单独读走：MCP 服务端常把日志写到 stderr，留着不读会把它自己的
        // 缓冲区写满、进而把整个进程卡死（这是个很隐蔽的坑）。
        if let Some(stderr) = child.stderr.take() {
            let logs = self.logs.clone();
            let name = server.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    logs.record("mcp", format!("[{name}] {line}"));
                }
            });
        }

        let session = Arc::new(Mutex::new(Session {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        }));
        sessions.insert(server.id.clone(), session.clone());
        Ok(session)
    }

    /// 调用一个工具。
    pub async fn call(
        &self,
        server: &McpServerConfig,
        tool: &McpTool,
        args: &serde_json::Value,
    ) -> AppResult<String> {
        let session = self.session(server).await?;
        let mut guard = session.lock().await;

        // 会话可能已经死了（服务端退出、被系统回收）—— 这时重连一次再试，
        // 而不是把这个错误直接甩给用户。
        let response = match guard
            .request("tools/call", call_params(&tool.raw_name, args))
            .await
        {
            Ok(response) => response,
            Err(_) => {
                drop(guard);
                self.sessions.lock().await.remove(&server.id);
                let session = self.session(server).await?;
                let mut guard = session.lock().await;
                guard
                    .request("tools/call", call_params(&tool.raw_name, args))
                    .await?
            }
        };

        match response {
            RpcMessage::Response { result, error, .. } => {
                tool_call_text(result.as_ref(), error.as_deref())
            }
            _ => Err(AppError::new(code::SERVER, "MCP 调用没有拿到应答")),
        }
    }

    /// 关掉所有会话（退出时用）。
    pub async fn shutdown(&self) {
        let mut sessions = self.sessions.lock().await;
        for (_, session) in sessions.drain() {
            let mut guard = session.lock().await;
            let _ = guard.child.start_kill();
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new(LogBuffer::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::config::McpServerConfig;

    /// 假 MCP 服务端的路径（见那个文件开头的说明）。
    fn fake_server(enabled: bool) -> McpServerConfig {
        McpServerConfig {
            id: "fake".to_string(),
            name: "假服务端".to_string(),
            command: "node".to_string(),
            args: vec![format!(
                "{}/tests/fixtures/fake-mcp-server.mjs",
                env!("CARGO_MANIFEST_DIR")
            )],
            env: Default::default(),
            enabled,
        }
    }

    /// 真拉起子进程、真走一遍协议。
    ///
    /// 这条测试的价值在于它验的是**真管道 + 真进程**：按行分帧、握手顺序、
    /// id 对应、通知要跳过、stderr 不能堵住 —— 这些用 mock 都验不出来。
    /// 需要一个能跑 `node` 的环境；没有就跳过（CI 上装个 node 即可）。
    #[tokio::test]
    async fn a_real_subprocess_handshake_list_and_call_work() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("跳过：这台机器上没有 node");
            return;
        }

        let manager = McpManager::new(LogBuffer::default());
        let statuses = manager.refresh(&[fake_server(true)]).await;
        assert_eq!(statuses.len(), 1);
        let status = &statuses[0];
        assert!(
            status.connected,
            "应当连上：{}",
            status.error.clone().unwrap_or_default()
        );
        // 服务端自报的信息要取回来（握手应答里就有）
        assert_eq!(status.server_name.as_deref(), Some("fake-mcp"));
        assert_eq!(status.server_version.as_deref(), Some("9.9.9"));

        // 工具清单：两个，且带点的名字被换成了合法字符
        assert_eq!(
            status.tools.len(),
            2,
            "应当发现两个工具：{:?}",
            status.tools
        );
        let names: Vec<&str> = status.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"mcp__fake__echo"));
        assert!(
            names.contains(&"mcp__fake__fs_read_file"),
            "带点的工具名要被换成合法字符：{names:?}"
        );

        // 调用一个工具（走真管道）
        let echo = manager
            .find_tool("mcp__fake__echo")
            .await
            .expect("应当找到");
        let text = manager
            .call(
                &fake_server(true),
                &echo,
                &serde_json::json!({ "text": "你好" }),
            )
            .await
            .expect("调用应当成功");
        assert!(text.contains("回声：你好"), "{text}");

        // 失败的结果要标成「工具执行失败」，而不是当成正常内容
        let read = manager
            .find_tool("mcp__fake__fs_read_file")
            .await
            .expect("应当找到");
        let text = manager
            .call(
                &fake_server(true),
                &read,
                &serde_json::json!({ "path": "/nope" }),
            )
            .await
            .expect("调用本身应当成功（错误在结果里）");
        assert!(text.starts_with("工具执行失败"), "{text}");
        assert!(text.contains("文件不存在"));

        // 不存在的工具：服务端回 JSON-RPC 错误 → 我们报错
        let mut ghost = read.clone();
        ghost.raw_name = "nope".to_string();
        assert!(manager
            .call(&fake_server(true), &ghost, &serde_json::json!({}))
            .await
            .is_err());

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_reports_an_error_instead_of_hanging() {
        let manager = McpManager::new(LogBuffer::default());
        let broken = McpServerConfig {
            id: "broken".to_string(),
            name: "起不来的服务端".to_string(),
            command: "definitely-not-a-real-command-xyz".to_string(),
            args: Vec::new(),
            env: Default::default(),
            enabled: true,
        };
        let statuses = manager.refresh(&[broken]).await;
        assert_eq!(statuses.len(), 1);
        assert!(!statuses[0].connected);
        assert!(statuses[0].error.is_some(), "要把失败原因带回来");
        // 连不上时不能留下工具
        assert!(manager.cached_tools().await.is_empty());
    }

    #[tokio::test]
    async fn a_disabled_server_is_never_started() {
        let manager = McpManager::new(LogBuffer::default());
        let statuses = manager.refresh(&[fake_server(false)]).await;
        assert!(statuses.is_empty(), "关掉的服务器不该被扫描");
        assert!(manager.cached_tools().await.is_empty());
    }
}
