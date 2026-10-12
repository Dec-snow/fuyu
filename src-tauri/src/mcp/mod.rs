//! MCP 客户端（stdio 传输）。
//!
//! 模块分工：
//! - [`protocol`]：JSON-RPC 帧、工具名映射、结果拍平 —— **纯逻辑**，可单测；
//! - [`client`]：子进程与常驻会话、握手、`tools/list`、`tools/call`。

pub mod client;
pub mod protocol;

pub use client::McpManager;
pub use protocol::{McpServerStatus, McpTool};

/// 最多同时配置几个服务器。
///
/// 不是随便定的：每个服务器都是一个常驻子进程，而它们提供的工具会**全部**
/// 出现在模型的工具清单里 —— 工具太多会明显拉低模型挑对工具的准确率。
pub const MAX_SERVERS: usize = 8;

/// 单个服务器的命令 / 参数长度上限（粘贴时很容易带进来一整段无关内容）。
pub const MAX_FIELD_CHARS: usize = 500;
