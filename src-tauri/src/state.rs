use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// 审批的作用域：仅这一次 / 本次会话 / 永久。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalScope {
    #[default]
    Once,
    Session,
    Always,
}

/// 用户对一次工具审批的裁决。
#[derive(Debug, Clone, Default)]
pub struct ApprovalVerdict {
    pub approved: bool,
    pub scope: ApprovalScope,
}

/// 等待审批的工具调用。键是 `request_id:call_id`。
pub struct PendingToolApproval {
    pub sender: oneshot::Sender<ApprovalVerdict>,
    /// 这次审批是否允许「记住」（本次会话 / 永久）。
    ///
    /// 前端会把「永久允许」按钮置灰，但**后端必须自己再判一次**：
    /// IPC 命令可以被直接调用，策略不能只活在前端。
    pub can_remember: bool,
}

/// 审批表：`request_id:call_id` → 待裁决的通道。
pub type PendingToolApprovals = Arc<Mutex<HashMap<String, PendingToolApproval>>>;
/// 「本次会话允许」的授权，键 `(approval_session_id, tool_name)`。
pub type SessionToolApprovals = Arc<Mutex<HashSet<(String, String)>>>;

/// 全局共享状态（Tauri 的 `manage`）。
///
/// 所有字段都刻意做成可 `Clone` 的 `Arc` 包裹类型：Tauri 命令里的
/// `State<'_, T>` 是一个借用，**不能**跨 `.await` 持有，也不能 move 进
/// `spawn`。真正要带进异步任务的是这些 `Arc` 的克隆。
pub struct AppState {
    /// 共享 HTTP 客户端。放在 `Mutex` 里是为了能在用户改代理设置后整体重建；
    /// 请求时只 clone 一次（`reqwest::Client` 内部就是 `Arc`，clone 很便宜），
    /// 绝不长时间持锁。
    pub http: Mutex<reqwest::Client>,
    /// 每个在跑的生成任务一个取消令牌，键是 `request_id`。
    /// `chat_cancel` 只是把对应令牌 flip 一下，流循环自己会退出。
    pub cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// 等待审批的工具调用，键 `request_id:call_id`。
    /// `chat_approve_tool` 通过这个表找到对应的 `oneshot` 并把它唤醒。
    pub tool_approvals: PendingToolApprovals,
    /// 「本次会话允许」的授权，键 `(approval_session_id, tool_name)`。
    /// 换会话就换 scope，即使应用不重启也不会沿用旧授权。
    pub session_tool_approvals: SessionToolApprovals,
    /// 有界诊断日志（见 [`crate::diagnostics`]）。
    ///
    /// 出错时往里记一条：界面上那条提示一刷新就没了，而「诊断信息」页
    /// 要能让用户把现场复制下来反馈。**只记错误码与信息，绝不记密钥或消息内容。**
    pub logs: crate::diagnostics::LogBuffer,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            // 配置还没读出来，先用默认值建一个；启动流程读到配置后会重建。
            http: Mutex::new(build_http_client(None, true)),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            tool_approvals: Arc::new(Mutex::new(HashMap::new())),
            session_tool_approvals: Arc::new(Mutex::new(HashSet::new())),
            logs: crate::diagnostics::LogBuffer::new(),
        }
    }

    /// 取一份 HTTP 客户端的克隆。
    pub fn client(&self) -> reqwest::Client {
        self.http.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn rebuild_http_client(&self, proxy_url: Option<&str>, use_system_proxy: bool) {
        let client = build_http_client(proxy_url, use_system_proxy);
        *self.http.lock().unwrap_or_else(|e| e.into_inner()) = client;
    }

    pub fn register_cancellation(&self, request_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.cancellations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id.to_string(), token.clone());
        token
    }

    pub fn cancel(&self, request_id: &str) -> bool {
        let map = self.cancellations.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(request_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// 构造 `reqwest::Client`。
///
/// 关键取舍：**不设全局 `timeout`**。
/// SSE 流式回答可能跑好几分钟，一个总超时会在回答说到一半时把它掐死。
/// 只对「建连」阶段设 10 秒上限，运行期靠 [`CancellationToken`] 主动取消。
///
/// 代理的优先级：
/// 1. 显式配了 `proxy_url` —— 一律用它；
/// 2. 否则看 `use_system_proxy`：为真时保持 reqwest 的默认行为（读
///    `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` 以及系统代理设置），
///    为假时用 `no_proxy()` **明确关掉**环境变量代理。
///
/// 这个开关以前只是被存进配置文件、从来没有被读过 —— 也就是说界面上勾不勾
/// 都一样。要么让它生效，要么就该把它从配置里删掉；这里选择让它生效。
fn build_http_client(proxy_url: Option<&str>, use_system_proxy: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .tcp_nodelay(true)
        .tcp_keepalive(Duration::from_secs(60))
        .pool_max_idle_per_host(8)
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent(concat!("FloatAI/", env!("CARGO_PKG_VERSION")));

    if let Some(url) = proxy_url.map(str::trim).filter(|u| !u.is_empty()) {
        match reqwest::Proxy::all(url) {
            Ok(proxy) => builder = builder.proxy(proxy),
            Err(e) => eprintln!("代理地址无效，已忽略：{e}"),
        }
    } else if !use_system_proxy {
        builder = builder.no_proxy();
    }

    builder.build().expect("构建 HTTP 客户端失败")
}
