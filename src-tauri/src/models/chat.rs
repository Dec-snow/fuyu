//! 面向前端的数据结构：请求负载、流式事件、以及历史记录。

use serde::{Deserialize, Serialize};

use crate::errors::AppError;

/// 一条对话消息。`role` 取 `system` / `user` / `assistant` / `tool`。
///
/// 派生 `PartialEq` 是为了让上下文裁剪、请求体构造这些纯函数能在测试里
/// 直接比较整段消息（而不是逐字段断言）。
///
/// 前端只发 `role` + `content`；`tool_calls` / `tool_call_id` / `name` 是
/// **agent 循环内部**用的字段（工具调用与结果回灌），它们只存在于 Rust 侧的
/// 内存历史里，从不下发到前端、也不落库。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// 这条消息在数据库里的 id（前端回传历史时带上）。
    ///
    /// **图片不走 IPC**：前端只回传 id，后端拿它去库里取 `images_json`。
    /// 否则一个带图的多轮会话，每次提问都要在进程间搬几 MB 的 base64。
    #[serde(default)]
    pub id: Option<String>,
    /// data URL 形式的图片。前端通常不传，由后端按 `id` 从库里补齐。
    #[serde(default)]
    pub images: Option<Vec<String>>,
    /// `assistant` 消息发起工具调用时非空。
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// `tool` 消息回填时指向对应的 `tool_calls.id`。
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// `tool` 消息的工具名（回灌时补全，供展示 / 调试）。
    #[serde(default)]
    pub name: Option<String>,
}

/// `chat_send` 的入参。
///
/// 注意 API Key **不在这里**：它由 Rust 侧按 `provider_id` 从系统凭据管理器
/// 取出，前端从头到尾拿不到它。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendRequest {
    pub provider_id: String,
    pub model_key: String,
    pub messages: Vec<ChatMessage>,
    /// 追加到 messages 最前面的系统提示词。
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// 有值时，本轮问答会写入该会话的历史。
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// 前端生成的请求 ID。
    ///
    /// 为什么不让后端生成：`chat_send` 返回之前后端就已经开始 emit 事件了，
    /// 若 ID 由后端随机生成，前端可能在拿到返回值之前就收到首个 delta，
    /// 从而找不到归属。让前端先生成 ID 再传进来，这个竞态就不存在。
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 是否开启工具调用（agent 循环）。开启后请求会带上 `tools`，模型可以
    /// 选择调用内置工具，工具结果会回灌并继续生成，直到它不再调用工具为止。
    #[serde(default)]
    pub enable_tools: bool,
    /// 审批会话作用域。「本次会话允许」按这个 id 隔离 —— 换一个会话就换
    /// 一个 scope，即使应用没重启，也不会沿用旧会话的授权。
    #[serde(default)]
    pub approval_session_id: Option<String>,
}

/// 模型请求的一次工具调用（原生 function calling 的累积结果）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 序列化后的参数 JSON。
    pub arguments: String,
}

/// 一条历史会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model_key: String,
    pub system_prompt: Option<String>,
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// 列表里显示的条数，由查询时 JOIN 得到。
    #[serde(default)]
    pub message_count: i64,
    /// 较早历史的滚动摘要（空串表示还没有摘要）。
    #[serde(default)]
    pub summary: String,
    /// 摘要已经覆盖了前面多少条消息。
    #[serde(default)]
    pub summary_message_count: i64,
    /// 摘要覆盖到的**最后一条消息 id**。
    ///
    /// 切上下文时按这个 id 定位，而不是按条数 —— 条数是数据库行数，
    /// 与前端发来的数组并不等长（前端会过滤空消息，删除消息也不会回退计数）。
    #[serde(default)]
    pub summary_covered_message_id: Option<String>,
}

/// 一条消息的终态。
///
/// 失败与取消**也要落库**：否则下次打开会话，那条失败的对话就凭空消失了，
/// 用户既看不到「为什么没答出来」，也没有「重试」的锚点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Ok,
    Cancelled,
    Error,
}

impl MessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageStatus::Ok => "ok",
            MessageStatus::Cancelled => "cancelled",
            MessageStatus::Error => "error",
        }
    }

    /// 从数据库里的字符串还原；未知值（例如将来版本写的）回落为 `ok`，
    /// 不会因为一条脏数据让整个会话读不出来。
    pub fn from_db(value: &str) -> Self {
        match value {
            "cancelled" => MessageStatus::Cancelled,
            "error" => MessageStatus::Error,
            _ => MessageStatus::Ok,
        }
    }
}

/// `#[serde(default)]` 需要它；缺失字段一律当作 `ok`。
impl Default for MessageStatus {
    fn default() -> Self {
        MessageStatus::Ok
    }
}

/// 一条历史消息（落库后的形态：带 id 与时间戳）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub provider_id: Option<String>,
    pub model_key: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub created_at: i64,
    /// 终态；用户消息永远是 `ok`。
    #[serde(default)]
    pub status: MessageStatus,
    /// 仅当 `status == error` 时有值。
    #[serde(default)]
    pub error: Option<AppError>,
    /// data URL 形式的图片（只有用户消息可能有）。
    #[serde(default)]
    pub images: Vec<String>,
}

/// 供滚动摘要使用的轻量消息视图。
///
/// 刻意**不含图片字节**：摘要只需要文字，而 `images_json` 里的 base64
/// 可能有几十 MB。`has_images` 用来给「只发图不写字」的消息留个痕迹，
/// 否则它在摘要提示词里会变成一条空消息被跳过。
#[derive(Debug, Clone, PartialEq)]
pub struct MessageText {
    pub id: String,
    pub role: String,
    pub content: String,
    pub has_images: bool,
}

/// 正文搜索的命中项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageHit {
    pub message_id: String,
    pub conversation_id: String,
    pub conversation_title: String,
    pub role: String,
    /// 命中片段，命中关键词两侧带 `[` `]` 标记。
    pub snippet: String,
    pub created_at: i64,
}

/// 流式事件：[`crate::commands::chat`] 每产生一段内容就往前端推一个。
///
/// 前端**只**依赖这套归一化事件，永远不接触厂商原始 JSON —— 换厂商时只需
/// 改 Rust 侧适配器，前端一行都不用动。
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ChatEvent {
    /// 请求已发出，前端可以进入“生成中”状态。
    Start { request_id: String },
    /// 正文增量。
    Delta { request_id: String, text: String },
    /// 思考过程增量（DeepSeek-R1 等模型的 reasoning_content）。
    ReasoningDelta { request_id: String, text: String },
    /// 上下文超出预算，较早的消息被省略了。
    ///
    /// 必须让用户看见：静默丢弃历史会让人以为模型「忘了」前面说过的话。
    ContextTrimmed {
        request_id: String,
        /// 真正被**丢掉**的消息条数（预算装不下）。
        dropped_messages: usize,
        /// 改用**摘要**替代的消息条数。与上一项不同：这部分信息没丢。
        summarized_messages: usize,
        /// 裁剪后估算的总 token。
        estimated_tokens: usize,
    },
    /// 滚动摘要已更新（后台压缩完成）。
    ///
    /// 前端据此在会话顶部展示「历史摘要」，让用户知道较早的内容被压缩过。
    /// 带上 `conversation_id` 是必须的：这个事件在终态事件**之后**才发，
    /// 那时前端已经按 `request_id` 清掉了路由表，光靠 request_id 找不到
    /// 它属于哪个会话。
    SummaryUpdated {
        request_id: String,
        conversation_id: String,
        summary: String,
        covered_messages: usize,
    },
    /// token 用量，通常在流末尾。
    Usage {
        request_id: String,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    /// 正常收流。
    Finish {
        request_id: String,
        reason: Option<String>,
    },
    /// 用户主动取消。
    Cancelled { request_id: String },
    /// 出错。`retryable` 告诉前端要不要给“重试”按钮。
    Error {
        request_id: String,
        code: String,
        message: String,
        retryable: bool,
    },
    /// 工具开始执行。
    ToolStart {
        request_id: String,
        call_id: String,
        name: String,
        /// 序列化后的参数 JSON（给人看）。
        arguments: String,
    },
    /// 工具执行完成。
    ToolEnd {
        request_id: String,
        call_id: String,
        name: String,
        result: String,
    },
    /// 工具执行失败。
    ToolError {
        request_id: String,
        call_id: String,
        name: String,
        message: String,
    },
    /// 敏感工具需要用户审批。
    ToolPending {
        request_id: String,
        call_id: String,
        name: String,
        /// 给人看的参数摘要。
        summary: String,
        /// 是否允许「本次会话 / 永久」记住。数据外发类工具永远 false。
        can_remember: bool,
    },
    /// 用户拒绝了工具调用。
    ToolRejected {
        request_id: String,
        call_id: String,
        name: String,
    },
}

/// 给每个事件套一个单调递增的序号后再发出去。
///
/// 前端可以据此去重、发现空洞、以及在乱序到达时重新排序 —— IPC 通道在
/// 高压下并不保证绝对有序。
#[derive(Debug, Clone, Serialize)]
pub struct SequencedChatEvent {
    pub seq: u64,
    #[serde(flatten)]
    pub event: ChatEvent,
}
