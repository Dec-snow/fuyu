//! 对话命令：Tauri 管道层。
//!
//! 这里只做四件事：**参数校验 → 解析配置与密钥 → 调 `api::stream_client`
//! 的纯核心 → 落库并向前端广播终态事件**。
//!
//! 真正的流式逻辑（重试策略、SSE 消费、`finish_reason` 宽限期、取消）
//! 全在 [`crate::api::stream_client`]，那里没有 `AppHandle` 也没有
//! `Database`，因此能对着一个本地假 SSE 服务端做端到端测试。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use crate::agent::{context, summary};
use crate::api::stream_client::{self, StreamOutcome};
use crate::api::{complete, openai};
use crate::errors::{code, AppError, AppResult};
use crate::models::chat::{
    ChatEvent, ChatMessage, ChatSendRequest, MessageStatus, SequencedChatEvent, ToolCall,
};
use crate::state::{
    AppState, ApprovalScope, ApprovalVerdict, PendingToolApproval, PendingToolApprovals,
    SessionToolApprovals,
};
use crate::storage::config::{AppConfig, ConfigStore, ProviderConfig};
use crate::storage::database::{Database, DEFAULT_CONVERSATION_TITLE};
use crate::storage::{new_id, secrets};
use crate::tools::registry::{self, ToolDefinition};

/// 所有流式事件都走这一个通道。
pub const EVENT_CHANNEL: &str = "chat-event";

const MAX_CHAT_MESSAGES: usize = 200;
const MAX_MESSAGE_CHARS: usize = 200_000;
/// 系统提示词的字符上限。
const MAX_SYSTEM_PROMPT_CHARS: usize = 20_000;
/// 「测试连接」的整体超时。
const TEST_TIMEOUT_SECS: u64 = 30;
/// agent 循环最多跑几轮（模型连续调用工具的轮数上限）。
const MAX_AGENT_STEPS: usize = 5;
/// 敏感工具审批的最长等待时间，超时视为拒绝。
const APPROVAL_TIMEOUT_SECS: u64 = 60;
/// 生成滚动摘要时允许的最大输出长度（摘要本身不该很长）。
const SUMMARY_MAX_TOKENS: u32 = 800;
/// 生成滚动摘要的请求超时。
const SUMMARY_TIMEOUT_SECS: u64 = 45;
/// 单个工具结果回灌给模型时的字符上限。
///
/// `fetch_webpage` 最多能返回 512 KB 正文，原样塞回历史会让**下一轮**请求远超
/// 上下文预算（十万 token 量级），服务端直接 400 —— 而工具其实已经执行过了，
/// 用户只看到一个「请求被拒绝」，还不知道是自己刚批准的那个抓取干的。
/// 截断并明确告诉模型「内容被截断了」，它就知道该怎么继续（换更具体的问题、
/// 或者抓另一个页面）。
const MAX_TOOL_RESULT_CHARS: usize = 8_000;

// ------------------------------------------------------------------ 命令入口

/// 发起一次流式对话。
///
/// **立刻返回 `request_id`**，真正的网络与解析在后台任务里跑；前端拿到 ID
/// 后靠 `chat-event` 通道收数据。
///
/// 声明成 `async` 是有意的：Tauri 里**非 async 的命令跑在主线程上**，而这个
/// 函数要读系统凭据管理器（一次 OS 调用，可能十几毫秒）和配置文件，放在主线程
/// 会直接卡住界面渲染。`async` 命令会丢到 tokio 运行时上执行。
#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    config: State<'_, ConfigStore>,
    db: State<'_, Database>,
    request: ChatSendRequest,
) -> AppResult<String> {
    validate_request(&request)?;

    // 1) 解析服务商与模型。
    let (provider, effective_system_prompt) = config.read(|cfg| {
        let provider = cfg.provider(&request.provider_id).cloned();
        let prompt = resolve_system_prompt(cfg, &request);
        (provider, prompt)
    });
    let provider = provider
        .ok_or_else(|| AppError::invalid(format!("找不到服务商：{}", request.provider_id)))?;

    let model = if request.model_key.trim().is_empty() {
        provider
            .models
            .first()
            .map(|m| m.key.clone())
            .ok_or_else(|| AppError::invalid("该服务商还没有可用模型"))?
    } else {
        request.model_key.clone()
    };

    // 2) 密钥只在 Rust 侧出现，且不会进入任何日志或错误消息。
    let api_key = secrets::get_api_key(&provider.id).map_err(AppError::secret)?;
    let api_key = api_key.unwrap_or_default();
    if api_key.is_empty() && !is_keyless_provider(&provider) {
        return Err(AppError::new(
            code::UNAUTHORIZED,
            format!(
                "{} 还没有配置 API Key，请到「设置 → 模型」里填写",
                provider.name
            ),
        ));
    }

    let url = openai::chat_completions_url(&provider.base_url)?;
    let request_id = request
        .request_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| new_id("req_"));

    // 3) 注册取消令牌，并把异步任务需要的所有东西 clone 出来。
    let token = state.register_cancellation(&request_id);
    let client = state.client();
    let cancellations = state.cancellations.clone();
    let tool_approvals = state.tool_approvals.clone();
    let session_tool_approvals = state.session_tool_approvals.clone();
    // `State<'_, Database>` / `State<'_, ConfigStore>` 是借用，不能 move 进
    // spawn；`inner()` 拿到真正的值，再 clone 出 `Arc` 的副本。
    let db = db.inner().clone();
    let config_store = config.inner().clone();

    let mut request = request;
    request.system_prompt = effective_system_prompt;

    let rid = request_id.clone();
    let cleanup_id = rid.clone();
    let task_token = token.clone();
    let context_budget = config.read(|c| c.context_budget_tokens as usize);
    let logs = state.logs.clone();
    tauri::async_runtime::spawn(async move {
        run_stream(
            app,
            client,
            db,
            request,
            provider,
            api_key,
            url,
            model,
            rid,
            task_token,
            context_budget,
            tool_approvals,
            session_tool_approvals,
            config_store,
            logs,
        )
        .await;
        // 任务无论怎么结束都要把令牌摘掉，否则 map 会一直涨。
        cancellations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cleanup_id);
    });

    Ok(request_id)
}

/// 取消一次正在跑的生成。
///
/// 返回 `true` 表示确实找到并取消了一个任务；`false` 表示它已经结束了。
#[tauri::command]
pub fn chat_cancel(state: State<'_, AppState>, request_id: String) -> bool {
    state.cancel(&request_id)
}

/// 一次审批最终生效的作用域。
///
/// 不允许「记住」的审批（数据外发类工具、以及「发送本地数据给模型」）在这里
/// **降级成「仅这一次」**。前端虽然把按钮置灰了，但这条命令本身可以被直接调用；
/// 「每次联网都要用户看着 URL 点头」这条防线必须落在后端。
fn effective_scope(requested: ApprovalScope, can_remember: bool) -> ApprovalScope {
    if can_remember {
        requested
    } else {
        ApprovalScope::Once
    }
}

/// 用户对一次敏感工具审批的裁决。
///
/// `remember` 取值 `once` / `session` / `always`；`approved` 为假时忽略 scope。
/// 返回 `false` 表示审批已经超时（或被取消），裁决作废。
#[tauri::command]
pub fn chat_approve_tool(
    state: State<'_, AppState>,
    request_id: String,
    call_id: String,
    approved: bool,
    remember: String,
) -> bool {
    let scope = match remember.as_str() {
        "session" => ApprovalScope::Session,
        "always" => ApprovalScope::Always,
        _ => ApprovalScope::Once,
    };
    let key = format!("{request_id}:{call_id}");
    let pending = state
        .tool_approvals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);

    match pending {
        Some(entry) => {
            let scope = effective_scope(scope, entry.can_remember);
            let _ = entry.sender.send(ApprovalVerdict { approved, scope });
            true
        }
        None => false,
    }
}

/// 用一次极短的非流式请求验证「Base URL + Key + 模型」三者是否可用。
#[tauri::command]
pub async fn test_provider(
    state: State<'_, AppState>,
    config: State<'_, ConfigStore>,
    provider_id: String,
    model_key: String,
) -> AppResult<String> {
    let provider = config
        .read(|c| c.provider(&provider_id).cloned())
        .ok_or_else(|| AppError::invalid("找不到该服务商"))?;
    let api_key = secrets::get_api_key(&provider.id)
        .map_err(AppError::secret)?
        .unwrap_or_default();
    let url = openai::chat_completions_url(&provider.base_url)?;
    let client = state.client();

    let body = serde_json::json!({
        "model": model_key,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 4,
        "stream": false
    });

    // 复用 `complete::complete_once`：非流式请求的超时、状态码与 JSON 提取
    // 只有这一处实现，别再写第二份（滚动摘要走的是同一条路径）。
    let text = complete::complete_once(
        &client,
        &url,
        &api_key,
        &body,
        Duration::from_secs(TEST_TIMEOUT_SECS),
    )
    .await?;

    Ok(format!("连接正常，模型回复：{}", text.trim()))
}

// ------------------------------------------------------------------ 主流程

/// 一轮完整的生成：建连 → 解析流 →（agent 循环：执行工具）→ 落库 → 广播终态。
///
/// 当 `request.enable_tools` 为真时，请求会带上 `tools`；模型返回 `tool_calls`
/// 时，这里逐个执行（敏感工具先审批），把结果回灌成 `tool` 消息再问一轮，
/// 直到模型不再调用工具、或者达到 [`MAX_AGENT_STEPS`]。
#[allow(clippy::too_many_arguments)]
async fn run_stream(
    app: AppHandle,
    client: reqwest::Client,
    db: Database,
    request: ChatSendRequest,
    provider: ProviderConfig,
    api_key: String,
    url: String,
    model: String,
    request_id: String,
    token: tokio_util::sync::CancellationToken,
    context_budget: usize,
    tool_approvals: PendingToolApprovals,
    session_tool_approvals: SessionToolApprovals,
    config: ConfigStore,
    logs: crate::diagnostics::LogBuffer,
) {
    let emitter = EventEmitter::new(app);

    // Start 必须在发请求之前发出去，前端才能立刻进入「生成中」状态。
    emitter.emit(ChatEvent::Start {
        request_id: request_id.clone(),
    });

    // 上下文预算：前端每次都把整个会话历史发过来，聊久了必然装不下。
    //
    // 先看看这个会话有没有滚动摘要：有的话，被摘要覆盖过的那段原文就不再
    // 逐条发送，改用摘要代替 —— 比直接丢掉温和得多。
    //
    // 注意第三个参数是**消息 id**而不是条数：按条数切会在「前端过滤了空消息」
    // 或「用户删过消息」时错位，把用户刚问的那句话当成已覆盖而丢掉。
    let (summary_text, _summary_count, summary_covered_id) =
        load_summary(&db, request.conversation_id.as_deref());
    let fitted = context::fit_context_with_summary(
        &request.messages,
        &summary_text,
        summary_covered_id.as_deref(),
        request.system_prompt.as_deref(),
        context_budget,
    );
    if fitted.was_trimmed() || fitted.summarized > 0 {
        emitter.emit(ChatEvent::ContextTrimmed {
            request_id: request_id.clone(),
            dropped_messages: fitted.dropped,
            summarized_messages: fitted.summarized,
            estimated_tokens: fitted.estimated_tokens,
        });
    }

    // agent 循环的消息历史：从裁剪后的历史出发，逐步追加 assistant / tool 消息。
    let mut history = fitted.messages;
    // 裁剪之后再补图片：被裁掉的消息不必去查图。
    attach_images(&db, &mut history);

    // 联网搜索没配端点，就**不下发**这个工具 —— 模型看不到就不会去调。
    let search_endpoint = config
        .read(|c| c.web_search_endpoint.clone())
        .filter(|value| !value.trim().is_empty());
    // 联网工具不复用共享客户端（要逐跳钉死 IP），所以代理得单独传给它们，
    // 否则「聊天走代理、抓网页直连」——在必须走代理的网络里工具全废。
    let proxy_url = config
        .read(|c| c.proxy_url.clone())
        .filter(|value| !value.trim().is_empty());
    // 知识库为空时不下发检索工具：模型看不到就不会去调一个必然空手而归的工具。
    // 这里每次请求都查一次 COUNT —— 单表计数，代价可以忽略。
    let kb_available = db.kb_document_count().unwrap_or(0) > 0;
    let tools = request.enable_tools.then(|| {
        registry::to_openai_tools(registry::ToolAvailability {
            web_search: search_endpoint.is_some(),
            knowledge_base: kb_available,
        })
    });
    let tool_ctx = registry::ToolContext {
        search_endpoint: search_endpoint.clone(),
        proxy_url,
        // 库里没文档就不给句柄：与「不下发工具」保持一致，
        // 免得出现「工具列表里没有、但直接调用却能跑」的不一致状态。
        knowledge_base: kb_available.then(|| db.clone()),
    };

    // 数据流防泄漏：任何「读本地敏感数据」的工具成功执行过之后置位；
    // 下一轮把上下文发给模型服务商前，必须让用户再确认一次。
    let mut context_sensitive = false;

    // 多轮流式的产出都并进这里（见 `TurnAccumulator::absorb`）。
    let mut turn = TurnAccumulator::default();
    let mut terminal_error: Option<AppError> = None;
    let mut cancelled = false;
    // 循环是**正常结束**的吗（模型不再调工具了）？还是把 5 轮用满了？
    let mut completed = false;

    for _step in 0..MAX_AGENT_STEPS {
        if token.is_cancelled() {
            cancelled = true;
            break;
        }

        // 数据流防泄漏：本地敏感数据已经读进来了，再发给模型服务商前确认一次。
        if context_sensitive
            && !confirm_provider_send(&tool_approvals, &emitter, &request_id, &token).await
        {
            terminal_error = Some(AppError::new(
                code::CANCELLED,
                "已取消：你拒绝了把本地数据发送给模型服务商",
            ));
            cancelled = true;
            break;
        }

        let body = openai::build_stream_body(
            &model,
            &history,
            request.system_prompt.as_deref(),
            request.temperature,
            request.max_tokens,
            tools.as_ref(),
        );

        let response =
            match stream_client::send_with_retry(&client, &url, &api_key, &body, &token).await {
                Ok(response) => response,
                Err(e) => {
                    terminal_error = Some(e);
                    break;
                }
            };

        let outcome = stream_client::consume_stream(response, &request_id, &token, |event| {
            emitter.emit(event)
        })
        .await;

        // 累积 + 判断这一轮的去向。**顺序固化在 `absorb` 里面**：
        // 取消 / 出错也必须先收下已经吐出来的内容再跳出，
        // 否则「停止」之后那半截回答不会落库（详见 `absorb` 的注释）。
        match turn.absorb(&outcome) {
            RoundFlow::Cancelled => {
                cancelled = true;
                break;
            }
            RoundFlow::Failed(e) => {
                terminal_error = Some(e);
                break;
            }
            RoundFlow::Done => {
                // 没有工具调用 → 这就是最终回答，循环正常结束。
                completed = true;
                break;
            }
            RoundFlow::Tools => {}
        }

        // 记录这一轮 assistant 发起的工具调用。
        //
        // **正文必须一起带上**：模型经常在调用工具前先说明一句
        // （「我先看一下你的剪贴板」），那段话已经推给前端、也会落库。
        // 如果这里塞空串，模型在后续轮次里就看不到自己说过什么 ——
        // 轻则重复叙述，重则前后矛盾；个别服务端还会因为
        // 「assistant 消息既没有 content 也没有 tool_calls 之外的上下文」而报参数错。
        history.push(ChatMessage {
            role: "assistant".to_string(),
            content: outcome.content.clone(),
            tool_calls: Some(outcome.tool_calls.clone()),
            ..Default::default()
        });

        // 逐个执行工具。
        for call in &outcome.tool_calls {
            if token.is_cancelled() {
                cancelled = true;
                break;
            }

            let Some(tool) = registry::find(&call.name) else {
                emitter.emit(ChatEvent::ToolError {
                    request_id: request_id.clone(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    message: "未知工具".to_string(),
                });
                // 即使没执行也必须回灌一条 tool 消息，否则下一轮会 400。
                history.push(tool_message(
                    &call.id,
                    &call.name,
                    format!("错误：未知工具 {}", call.name),
                ));
                continue;
            };

            let approved = request_approval(
                &tool_approvals,
                &session_tool_approvals,
                &config,
                &emitter,
                &request_id,
                call,
                &tool,
                request.approval_session_id.as_deref(),
                &token,
                &logs,
            )
            .await;

            if !approved {
                emitter.emit(ChatEvent::ToolRejected {
                    request_id: request_id.clone(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                });
                history.push(tool_message(
                    &call.id,
                    &call.name,
                    "用户拒绝了此操作".to_string(),
                ));
                continue;
            }

            emitter.emit(ChatEvent::ToolStart {
                request_id: request_id.clone(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });

            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));

            // 执行也要能被取消。联网工具最长可以跑十几秒（单跳 15s × 最多 5 跳），
            // 不 race 的话用户点了「停止」要干等十几秒才有反应，
            // 而且像「写剪贴板」这种**有副作用**的工具在取消后照样会生效。
            let result = tokio::select! {
                _ = token.cancelled() => {
                    cancelled = true;
                    break;
                }
                result = registry::execute(&call.name, &args, &tool_ctx) => result,
            };

            let clipped = clip_tool_result(&result.content);

            if result.touched_local_data {
                context_sensitive = true;
            }

            if result.is_error {
                emitter.emit(ChatEvent::ToolError {
                    request_id: request_id.clone(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    message: clipped.clone(),
                });
            } else {
                emitter.emit(ChatEvent::ToolEnd {
                    request_id: request_id.clone(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    result: clipped.clone(),
                });
            }
            // 回灌给模型的同样用截断版：一个几百 KB 的网页能把下一轮直接顶爆。
            history.push(tool_message(&call.id, &call.name, clipped));
        }

        if cancelled {
            break;
        }
    }

    // 把 5 轮用满了：最后一轮的工具有结果，但模型没有机会基于它们作答。
    //
    // 必须明确报错，不能当作「正常完成」—— 否则（尤其是这几轮一个字都没生成时）
    // 前端只收到一个 Finish，气泡在结算时直接消失：用户既没看到回答，
    // 也没有任何可重试的锚点，只会以为应用坏了。
    if !completed && terminal_error.is_none() && !cancelled {
        terminal_error = Some(AppError::new(
            code::INTERNAL,
            format!(
                "工具调用达到 {MAX_AGENT_STEPS} 轮上限，本轮没能得出最终回答。\
                 可以接着追问，或者把问题拆小一点。"
            ),
        ));
    }

    // 组装一个 StreamOutcome 复用落库逻辑（工具循环的产出同样走这一套）。
    let outcome = StreamOutcome {
        content: turn.content,
        reasoning: turn.reasoning,
        finish_reason: turn.finish_reason.clone(),
        usage: turn.usage,
        cancelled,
        error: terminal_error.clone(),
        tool_calls: Vec::new(),
    };

    // 落库要在发终态事件**之前**：前端收到 Finish 时去刷新会话列表，
    // 那时历史必须已经写好了。
    if let Some(conversation_id) = request.conversation_id.as_deref() {
        // `terminal_error` 里也包含「用户拒绝把本地数据发给服务商」——它的 code
        // 是 CANCELLED，终端事件发的也是 `Cancelled`。落库必须跟它一致：
        // 否则气泡先显示「已停止生成」，结算回读数据库后又变成一个红色错误条，
        // 同一个动作冒出两种终态。
        let status = match terminal_error.as_ref() {
            Some(e) if e.code == code::CANCELLED => MessageStatus::Cancelled,
            Some(_) => MessageStatus::Error,
            None if cancelled => MessageStatus::Cancelled,
            None => MessageStatus::Ok,
        };
        persist_turn(
            &db,
            conversation_id,
            &outcome,
            &provider.id,
            &model,
            &request,
            status,
            terminal_error.as_ref(),
        );
    }

    // 终态发完之后再做摘要。先在这里定下「这轮是否成功」—— 下面 `match`
    // 会把 `terminal_error` move 走，之后就问不出来了。
    let succeeded = terminal_error.is_none() && !cancelled;

    let terminal = match terminal_error {
        Some(e) => {
            // 记一条诊断日志。放在这里而不是错误产生的地方：无论错误来自建连、
            // SSE 解析还是「工具轮次用满」，终态只有这一处，记在这里一处就够。
            // **只记服务商 / 模型 / 错误码与信息** —— 诊断信息是要给用户复制
            // 出去给人看的，绝不能带上消息内容。
            logs.record("chat", format!("{} · {} · {e}", provider.id, model));
            terminal_event(&request_id, Err(e))
        }
        None if cancelled => ChatEvent::Cancelled {
            request_id: request_id.clone(),
        },
        None => ChatEvent::Finish {
            request_id: request_id.clone(),
            reason: turn.finish_reason,
        },
    };
    emitter.emit(terminal);

    // 只有这一轮真的成功了才值得压缩。这一步要额外发一次 API 请求，所以放在
    // 终态之后 —— 不能拖慢用户看到回答结束的那一刻。
    if succeeded {
        if let Some(conversation_id) = request.conversation_id.as_deref() {
            maybe_summarize(
                &db,
                &client,
                &url,
                &api_key,
                &model,
                conversation_id,
                &emitter,
                &request_id,
                &token,
            )
            .await;
        }
    }
}

/// 读取会话的滚动摘要：`(摘要正文, 已覆盖条数, 覆盖到的最后一条消息 id)`。
///
/// 条数只用于界面展示（「较早的 N 条已压缩」）；**切上下文靠的是那个 id** ——
/// 条数是数据库行数，与前端发来的数组并不等长，用位置切会把用户的提问切掉。
///
/// 读不到（会话刚建 / 查询失败）就当作「没有摘要」，绝不让它影响主流程。
fn load_summary(db: &Database, conversation_id: Option<&str>) -> (String, usize, Option<String>) {
    let Some(id) = conversation_id else {
        return (String::new(), 0, None);
    };
    match db.get_conversation(id) {
        Ok(Some(conversation)) => (
            conversation.summary,
            conversation.summary_message_count.max(0) as usize,
            conversation.summary_covered_message_id,
        ),
        _ => (String::new(), 0, None),
    }
}

/// 会话变长之后，把最早的一段历史压成滚动摘要（失败不打扰用户）。
///
/// 「失败不打扰」是刻意的：摘要是**锦上添花**的优化，它挂了不该让一次
/// 已经成功的问答看起来像出了问题。所以这里只 `eprintln`，不发 Error 事件。
#[allow(clippy::too_many_arguments)]
async fn maybe_summarize(
    db: &Database,
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    conversation_id: &str,
    emitter: &EventEmitter,
    request_id: &str,
    token: &tokio_util::sync::CancellationToken,
) {
    // 只取「角色 + 正文 + 有没有图」，**不读图片字节** —— 摘要用不上它们，
    // 而一个带图的长会话能轻松到几十 MB。
    let Ok(texts) = db.message_texts(conversation_id) else {
        return;
    };
    let (previous, covered, _) = load_summary(db, Some(conversation_id));

    // 先把「给摘要看的正文」摊平：只发图不写字的消息留一句痕迹，
    // 否则 `build_summary_messages` 会因为正文为空把它整条跳过，
    // 摘要里就完全看不出「用户当时给过一张图」。
    let contents: Vec<String> = texts
        .iter()
        .map(|m| {
            if m.content.trim().is_empty() && m.has_images {
                "（发了一张图片）".to_string()
            } else {
                m.content.clone()
            }
        })
        .collect();

    let roles: Vec<&str> = texts.iter().map(|m| m.role.as_str()).collect();
    let Some(planned) = summary::plan_summary(&roles, covered) else {
        return;
    };
    // 再把区间收缩到「提示词装得下」：不收缩的话，一个贴过超长文档的会话
    // 会构造出永远失败的摘要请求，每轮白烧一次 API 调用，进度还永不前进。
    let Some(range) = summary::shrink_to_budget(
        &roles,
        &contents,
        planned,
        summary::MAX_SUMMARY_INPUT_TOKENS,
    ) else {
        return;
    };

    // 把这次折叠的「边界消息 id」记下来：下一轮切上下文靠它定位，
    // 而不是靠条数（条数是行数，和前端发来的数组并不等长）。
    let Some(boundary_id) = summary::covered_message_id(&texts, &range) else {
        // 区间里一条有内容的都没有，没有摘要的价值。
        return;
    };

    // 落库的消息只有 user / assistant 两种角色，直接转成请求消息即可。
    let slice: Vec<ChatMessage> = (range.start..range.end)
        .filter_map(|index| {
            Some(ChatMessage {
                role: texts.get(index)?.role.clone(),
                content: contents.get(index)?.clone(),
                ..Default::default()
            })
        })
        .collect();

    let messages = summary::build_summary_messages(&previous, &slice);
    let body = openai::build_completion_body(model, &messages, None, Some(SUMMARY_MAX_TOKENS));

    let text = tokio::select! {
        _ = token.cancelled() => return,
        result = complete::complete_once(
            client,
            url,
            api_key,
            &body,
            Duration::from_secs(SUMMARY_TIMEOUT_SECS),
        ) => match result {
            Ok(text) => text,
            Err(e) => {
                eprintln!("生成滚动摘要失败（不影响本轮回答）：{}", e.message);
                return;
            }
        },
    };

    let cleaned = summary::clean_summary(&text);
    if cleaned.is_empty() {
        return;
    }
    if let Err(e) = db.set_conversation_summary(
        conversation_id,
        &cleaned,
        range.end as i64,
        Some(boundary_id.as_str()),
    ) {
        eprintln!("写入滚动摘要失败：{e}");
        return;
    }

    emitter.emit(ChatEvent::SummaryUpdated {
        request_id: request_id.to_string(),
        conversation_id: conversation_id.to_string(),
        summary: cleaned,
        covered_messages: range.end,
    });
}

/// 截断过长的工具结果，并留下明确的截断标记。
///
/// 前端上只显示几十个字符，所以事件里也发截断后的版本 —— 没必要为了显示
/// 48 个字符把 512 KB 走一遍 IPC。
fn clip_tool_result(content: &str) -> String {
    if content.chars().count() <= MAX_TOOL_RESULT_CHARS {
        return content.to_string();
    }
    let mut out: String = content.chars().take(MAX_TOOL_RESULT_CHARS).collect();
    out.push_str("\n\n（内容过长，已截断。如需更多，请分次抓取或问得更具体一些。）");
    out
}

/// 组装一条 `tool` 消息（工具结果回灌）。
fn tool_message(call_id: &str, name: &str, content: String) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content,
        tool_call_id: Some(call_id.to_string()),
        name: Some(name.to_string()),
        ..Default::default()
    }
}

/// 三级授权：永久（配置）→ 会话（内存）→ 单次（审批）。
///
/// 返回 `true` 表示允许执行。敏感工具在「非永久、非会话」两档之外都必须走
/// 一次审批；`always` 档会写进 `providers.json`，`session` 档只存在内存里。
#[allow(clippy::too_many_arguments)]
async fn request_approval(
    tool_approvals: &PendingToolApprovals,
    session_tool_approvals: &SessionToolApprovals,
    config: &ConfigStore,
    emitter: &EventEmitter,
    request_id: &str,
    call: &ToolCall,
    tool: &ToolDefinition,
    approval_session_id: Option<&str>,
    token: &tokio_util::sync::CancellationToken,
    logs: &crate::diagnostics::LogBuffer,
) -> bool {
    // 1) 永久允许。
    if config.read(|c| c.always_allowed_tools.iter().any(|t| t == tool.name)) {
        return true;
    }
    // 2) 本次会话允许。
    if let Some(session) = approval_session_id {
        let allowed = session_tool_approvals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(session.to_string(), tool.name.to_string()));
        if allowed {
            return true;
        }
    }
    // 3) 非敏感工具直接放行。
    if !tool.sensitive {
        return true;
    }

    // 4) 走审批：注册 oneshot，emit ToolPending，等待裁决（带超时）。
    //
    // 外发型工具（联网抓取）**不允许**「永久允许」：它的 URL 由模型生成，
    // 被注入的模型可以把对话内容塞进查询串带走。每次都让用户看着 URL 点头，
    // 是唯一可靠的防线。
    let can_remember = !tool.exfiltrates;
    let (tx, rx) = oneshot::channel();
    let key = format!("{request_id}:{}", call.id);
    tool_approvals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            key.clone(),
            PendingToolApproval {
                sender: tx,
                can_remember,
            },
        );

    emitter.emit(ChatEvent::ToolPending {
        request_id: request_id.to_string(),
        call_id: call.id.clone(),
        name: tool.name.to_string(),
        summary: summarize_arguments(&call.arguments),
        can_remember,
    });

    let verdict = tokio::select! {
        _ = token.cancelled() => None,
        result = rx => result.ok(),
        _ = tokio::time::sleep(Duration::from_secs(APPROVAL_TIMEOUT_SECS)) => None,
    };

    // 无论结果如何，摘掉审批表里的条目，别让它一直挂着。
    tool_approvals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);

    match verdict {
        Some(v) if v.approved => {
            match v.scope {
                ApprovalScope::Session => {
                    if let Some(session) = approval_session_id {
                        session_tool_approvals
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert((session.to_string(), tool.name.to_string()));
                    }
                }
                ApprovalScope::Always => {
                    // 写盘失败**必须留痕**。
                    //
                    // `ConfigStore::update` 在落盘失败时会把内存里的改动一起回滚，
                    // 所以用户点了「永久允许」之后会什么都不记住 —— 下一次同一个
                    // 工具还要再问一遍。如果这里悄悄吞掉错误，这个现象没有任何线索
                    // 可查（用户只会觉得「这软件记性有问题」）。
                    if let Err(e) = config.update(|cfg| {
                        if !cfg.always_allowed_tools.iter().any(|t| t == tool.name) {
                            cfg.always_allowed_tools.push(tool.name.to_string());
                        }
                        Ok(())
                    }) {
                        logs.record(
                            "config",
                            format!("记住「永久允许」（工具 {}）失败：{e}", tool.name),
                        );
                    }
                }
                ApprovalScope::Once => {}
            }
            true
        }
        _ => false,
    }
}

/// 数据流防泄漏：本地敏感工具执行过之后，本轮上下文再发给模型服务商前，
/// 必须再让用户确认一次。
///
/// 这一次确认伪装成一个「不可记住」的工具审批（`can_remember: false`），
/// 前端呈现为「是否允许把本地数据发送给模型」。拒绝即终止整个请求。
async fn confirm_provider_send(
    tool_approvals: &PendingToolApprovals,
    emitter: &EventEmitter,
    request_id: &str,
    token: &tokio_util::sync::CancellationToken,
) -> bool {
    let call_id = format!("provider-{request_id}");
    let key = format!("{request_id}:{call_id}");

    // 这个审批本身也永远不能「记住」：本地数据是否允许外发，必须每次都问。
    let can_remember = false;
    let (tx, rx) = oneshot::channel();
    tool_approvals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            key.clone(),
            PendingToolApproval {
                sender: tx,
                can_remember,
            },
        );

    emitter.emit(ChatEvent::ToolPending {
        request_id: request_id.to_string(),
        call_id: call_id.clone(),
        name: "发送本地数据至模型".to_string(),
        summary: "本轮对话已读取过本地敏感内容（如剪贴板），即将把它发送给模型服务商".to_string(),
        can_remember,
    });

    let verdict = tokio::select! {
        _ = token.cancelled() => None,
        result = rx => result.ok(),
        _ = tokio::time::sleep(Duration::from_secs(APPROVAL_TIMEOUT_SECS)) => None,
    };

    tool_approvals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);

    matches!(verdict, Some(v) if v.approved)
}

/// 把参数 JSON 截成一句人话，给审批卡片当摘要用。
fn summarize_arguments(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return "（无参数）".to_string();
    }
    if trimmed.chars().count() <= 160 {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(160).collect();
    out.push('…');
    out
}

fn terminal_event(request_id: &str, result: Result<(), AppError>) -> ChatEvent {
    match result {
        Ok(()) => ChatEvent::Finish {
            request_id: request_id.to_string(),
            reason: None,
        },
        Err(e) if e.code == code::CANCELLED => ChatEvent::Cancelled {
            request_id: request_id.to_string(),
        },
        Err(e) => ChatEvent::Error {
            request_id: request_id.to_string(),
            code: e.code,
            message: e.message,
            retryable: e.retryable,
        },
    }
}

/// 把这一轮问答写进历史：助手消息 + 首次自动命名 + 顶起 `updated_at`。
///
/// 失败与取消**也落库**：失败消息是「重试」的锚点，取消的半截回答用户已经
/// 看到了。唯一不落库的情形是「没生成任何正文」的成功/取消轮 —— 留一条空
/// 气泡没有任何意义。
#[allow(clippy::too_many_arguments)]
fn persist_turn(
    db: &Database,
    conversation_id: &str,
    outcome: &StreamOutcome,
    provider_id: &str,
    model: &str,
    request: &ChatSendRequest,
    status: MessageStatus,
    error: Option<&AppError>,
) {
    // 空正文的 ok / cancelled 轮不落库；error 轮即使空正文也要落（失败记录）。
    let has_content = outcome.has_content();
    if !has_content && status != MessageStatus::Error {
        return;
    }

    let input = outcome
        .usage
        .as_ref()
        .and_then(|u| u.input_tokens)
        .map(|v| v as i64);
    let output = outcome
        .usage
        .as_ref()
        .and_then(|u| u.output_tokens)
        .map(|v| v as i64);

    if let Err(e) = db.append_assistant_turn(
        conversation_id,
        &outcome.content,
        (!outcome.reasoning.is_empty()).then_some(outcome.reasoning.as_str()),
        provider_id,
        model,
        input,
        output,
        status,
        error,
    ) {
        eprintln!("写入助手消息失败：{e}");
        return;
    }

    // 首次对话自动用用户的第一句话给会话命名，省得列表里全是「新对话」。
    auto_title(db, conversation_id, request);
}

fn auto_title(db: &Database, conversation_id: &str, request: &ChatSendRequest) {
    let Ok(Some(conv)) = db.get_conversation(conversation_id) else {
        return;
    };
    if conv.title != DEFAULT_CONVERSATION_TITLE {
        return;
    }
    let Some(first_user) = request.messages.iter().find(|m| m.role == "user") else {
        return;
    };

    const MAX_TITLE_CHARS: usize = 24;
    let trimmed = first_user.content.trim();
    let mut title: String = trimmed.chars().take(MAX_TITLE_CHARS).collect();
    if trimmed.chars().count() > MAX_TITLE_CHARS {
        title.push('…');
    }
    if !title.is_empty() {
        let _ = db.rename_conversation(conversation_id, &title);
    }
}

// ------------------------------------------------------------------ 辅助

/// 把事件按序号发到前端通道。
///
/// `seq` 让前端能去重、发现空洞；IPC 通道在高并发下并不保证绝对有序。
struct EventEmitter {
    app: AppHandle,
    seq: AtomicU64,
}

impl EventEmitter {
    fn new(app: AppHandle) -> Self {
        Self {
            app,
            seq: AtomicU64::new(0),
        }
    }

    fn emit(&self, event: ChatEvent) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = self
            .app
            .emit(EVENT_CHANNEL, SequencedChatEvent { seq, event })
        {
            eprintln!("向前端推送事件失败：{e}");
        }
    }
}

/// 一轮流式产出该往哪走。
enum RoundFlow {
    /// 没有工具调用：这就是最终回答。
    Done,
    /// 有工具调用：执行它们，再问一轮。
    Tools,
    /// 用户点了停止。
    Cancelled,
    /// 出错了（`content` 里可能还有半截回答）。
    Failed(AppError),
}

/// 把多轮流式产出并成一份最终结果。
#[derive(Default)]
struct TurnAccumulator {
    content: String,
    reasoning: String,
    usage: Option<openai::Usage>,
    finish_reason: Option<String>,
}

impl TurnAccumulator {
    /// 收下一轮产出，并判断这一轮该往哪走。
    ///
    /// **累积发生在判断之前 —— 这是刻意的，而且是必须的。**
    /// 取消和出错都要保留已经吐出来的内容，因为 `persist_turn` 对
    /// 「空正文的非 error 轮」是直接 `return` 的：漏掉这一步，
    /// 用户点「停止」后屏幕上那半截回答就**不会落库** ——
    /// 切走再切回来（或重开应用）它就消失了，连「重试」的锚点都没有。
    /// （README 里「停止生成：已生成内容照常落库」这句承诺就靠它兑现。）
    ///
    /// 把顺序固化在函数内部，调用点就没机会写反 —— 这个 bug 当初就是
    /// 在调用点先 `break` 再累积造成的。
    fn absorb(&mut self, outcome: &StreamOutcome) -> RoundFlow {
        self.content.push_str(&outcome.content);
        self.reasoning.push_str(&outcome.reasoning);
        if outcome.usage.is_some() {
            self.usage = outcome.usage.clone();
        }
        if outcome.finish_reason.is_some() {
            self.finish_reason = outcome.finish_reason.clone();
        }

        if outcome.cancelled {
            return RoundFlow::Cancelled;
        }
        if let Some(e) = outcome.error.clone() {
            return RoundFlow::Failed(e);
        }
        if outcome.tool_calls.is_empty() {
            RoundFlow::Done
        } else {
            RoundFlow::Tools
        }
    }
}

fn validate_request(request: &ChatSendRequest) -> AppResult<()> {
    validate_messages(&request.messages)?;
    if request.provider_id.trim().is_empty() {
        return Err(AppError::invalid("未选择服务商"));
    }
    if let Some(prompt) = &request.system_prompt {
        if prompt.chars().count() > MAX_SYSTEM_PROMPT_CHARS {
            return Err(AppError::invalid(format!(
                "系统提示词过长（上限 {MAX_SYSTEM_PROMPT_CHARS} 字）"
            )));
        }
    }
    Ok(())
}

fn validate_messages(messages: &[ChatMessage]) -> AppResult<()> {
    if messages.is_empty() {
        return Err(AppError::invalid("消息列表为空"));
    }
    if messages.len() > MAX_CHAT_MESSAGES {
        return Err(AppError::invalid(format!(
            "单次请求最多 {MAX_CHAT_MESSAGES} 条消息，请新开一个对话"
        )));
    }
    for m in messages {
        if m.content.chars().count() > MAX_MESSAGE_CHARS {
            return Err(AppError::invalid("单条消息过长"));
        }
        if m.role != "user" && m.role != "assistant" && m.role != "system" {
            return Err(AppError::invalid(format!("不支持的消息角色：{}", m.role)));
        }
        // 正常情况下图片走 `save_user_message` 落库、再由 `attach_images` 按 id
        // 补回来；但前端**也**可以直接塞图片，所以这里同样要校验一遍 ——
        // 否则一条精心构造的 `data:text/html` 就能绕过落库那关。
        if let Some(images) = &m.images {
            crate::commands::history::validate_images(images)?;
        }
    }
    Ok(())
}

/// 把库里存的图片按消息 id 补回历史。
///
/// 前端回传历史时**只带 id、不带图片字节**：一个带图的多轮会话如果每次都把
/// base64 走一遍 IPC，几十 MB 的负载就白搬了。这里一次 `IN (...)` 查询补全，
/// 顺带避免了 N+1。
///
/// 查询失败**不**让整轮对话失败 —— 最坏的结果是这一轮模型看不到图，
/// 比直接报错好。
fn attach_images(db: &Database, messages: &mut [ChatMessage]) {
    let ids: Vec<String> = messages
        .iter()
        .filter(|m| m.images.is_none())
        .filter_map(|m| m.id.clone())
        .collect();
    if ids.is_empty() {
        return;
    }

    let Ok(stored) = db.images_for_ids(&ids) else {
        return;
    };

    for message in messages.iter_mut() {
        if message.images.is_some() {
            continue;
        }
        if let Some(id) = &message.id {
            if let Some(images) = stored.get(id) {
                message.images = Some(images.clone());
            }
        }
    }
}

/// 系统提示词优先级：请求 > 全局默认。
fn resolve_system_prompt(cfg: &AppConfig, request: &ChatSendRequest) -> Option<String> {
    if let Some(prompt) = request
        .system_prompt
        .as_ref()
        .filter(|p| !p.trim().is_empty())
    {
        return Some(prompt.clone());
    }
    if !cfg.system_prompt.trim().is_empty() {
        return Some(cfg.system_prompt.clone());
    }
    None
}

/// 本地服务（Ollama 等）允许不带 Key。
fn is_keyless_provider(provider: &ProviderConfig) -> bool {
    let base = provider.base_url.to_ascii_lowercase();
    provider.id == "ollama"
        || base.contains("localhost")
        || base.contains("127.0.0.1")
        || base.contains("0.0.0.0")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn request(messages: Vec<ChatMessage>) -> ChatSendRequest {
        ChatSendRequest {
            provider_id: "deepseek".to_string(),
            model_key: "deepseek-chat".to_string(),
            messages,
            system_prompt: None,
            conversation_id: None,
            request_id: None,
            temperature: None,
            max_tokens: None,
            enable_tools: false,
            approval_session_id: None,
        }
    }

    #[test]
    fn long_tool_results_are_clipped_for_the_model() {
        let short = "很短的结果";
        assert_eq!(clip_tool_result(short), short, "短结果不该被改动");

        let long = "字".repeat(MAX_TOOL_RESULT_CHARS + 5_000);
        let clipped = clip_tool_result(&long);

        assert!(
            clipped.chars().count() < long.chars().count(),
            "长结果必须截断"
        );
        assert!(clipped.starts_with(&"字".repeat(100)), "保留开头的内容");
        assert!(
            clipped.contains("已截断"),
            "要明确告诉模型内容被截断了，它才知道该怎么继续"
        );
        assert!(
            clipped.chars().count() <= MAX_TOOL_RESULT_CHARS + 100,
            "截断后不该仍是个巨型字符串"
        );
    }

    #[test]
    fn exfiltrating_tools_can_never_be_remembered() {
        // 即使请求「永久允许」，后端也把它降级成「仅这一次」。
        assert_eq!(
            effective_scope(ApprovalScope::Always, false),
            ApprovalScope::Once
        );
        assert_eq!(
            effective_scope(ApprovalScope::Session, false),
            ApprovalScope::Once
        );

        // 允许记住的工具照旧。
        assert_eq!(
            effective_scope(ApprovalScope::Always, true),
            ApprovalScope::Always
        );
        assert_eq!(
            effective_scope(ApprovalScope::Session, true),
            ApprovalScope::Session
        );
        assert_eq!(
            effective_scope(ApprovalScope::Once, true),
            ApprovalScope::Once
        );
    }

    #[test]
    fn a_cancelled_round_still_contributes_what_already_streamed() {
        // 这条守的是**实测抓到的真 bug**：如果先跳出循环、再累积这一轮的产出，
        // `final_content` 会永远是空的，而 `persist_turn` 对
        // 「空正文的非 error 轮」是直接 return 的 ——
        // 结果是用户点「停止」之后，屏幕上已经显示出来的那半截回答**不落库**，
        // 切走再切回来（或重开应用）它就消失了，连「重试」的锚点都没有。
        let mut turn = TurnAccumulator::default();
        let outcome = StreamOutcome {
            content: "第0片。第1片。".to_string(),
            cancelled: true,
            ..Default::default()
        };

        assert!(matches!(turn.absorb(&outcome), RoundFlow::Cancelled));
        assert_eq!(
            turn.content, "第0片。第1片。",
            "取消也必须保留已经生成的内容（README 承诺了「已生成内容照常落库」）"
        );
    }

    #[test]
    fn a_failed_round_still_contributes_what_already_streamed() {
        // 出错同理：错误轮会落库，但如果正文是空的，
        // 结算回读数据库之后气泡里的半截内容就会变没。
        let mut turn = TurnAccumulator::default();
        let outcome = StreamOutcome {
            content: "半截".to_string(),
            error: Some(AppError::new(code::NETWORK, "连接断了")),
            ..Default::default()
        };

        assert!(matches!(turn.absorb(&outcome), RoundFlow::Failed(_)));
        assert_eq!(turn.content, "半截");
    }

    #[test]
    fn multi_round_output_is_accumulated_in_order() {
        // agent 循环：中间轮可能只有 tool_calls、没有正文，但只要有正文
        // 就要按轮次拼起来（模型经常在调工具前先说一句）。
        let mut turn = TurnAccumulator::default();

        let with_tool = StreamOutcome {
            content: "我先看一下。".to_string(),
            tool_calls: vec![ToolCall {
                id: "c1".to_string(),
                name: "calculator".to_string(),
                arguments: "{}".to_string(),
            }],
            ..Default::default()
        };
        assert!(matches!(turn.absorb(&with_tool), RoundFlow::Tools));

        let final_round = StreamOutcome {
            content: "算完了。".to_string(),
            finish_reason: Some("stop".to_string()),
            ..Default::default()
        };
        assert!(matches!(turn.absorb(&final_round), RoundFlow::Done));

        assert_eq!(turn.content, "我先看一下。算完了。");
    }

    #[test]
    fn usage_and_finish_reason_survive_accumulation() {
        let mut turn = TurnAccumulator::default();
        let outcome = StreamOutcome {
            content: "答案".to_string(),
            finish_reason: Some("stop".to_string()),
            usage: Some(openai::Usage {
                input_tokens: Some(3),
                output_tokens: Some(4),
            }),
            ..Default::default()
        };

        assert!(matches!(turn.absorb(&outcome), RoundFlow::Done));
        assert_eq!(turn.usage.and_then(|u| u.output_tokens), Some(4));
        assert_eq!(turn.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn empty_message_list_is_rejected() {
        assert!(validate_messages(&[]).is_err());
    }

    #[test]
    fn unknown_role_is_rejected() {
        let messages = vec![ChatMessage {
            role: "tool".to_string(),
            content: "x".to_string(),
            ..Default::default()
        }];
        assert!(validate_messages(&messages).is_err());
    }

    #[test]
    fn too_many_messages_are_rejected() {
        let messages: Vec<_> = (0..MAX_CHAT_MESSAGES + 1)
            .map(|i| user(&i.to_string()))
            .collect();
        assert!(validate_messages(&messages).is_err());
    }

    #[test]
    fn system_prompt_falls_back_to_global_default() {
        let cfg = AppConfig::seeded();
        let req = request(vec![user("hi")]);
        assert_eq!(
            resolve_system_prompt(&cfg, &req),
            Some(cfg.system_prompt.clone())
        );

        let mut req2 = request(vec![user("hi")]);
        req2.system_prompt = Some("  专属人设  ".to_string());
        assert_eq!(
            resolve_system_prompt(&cfg, &req2),
            Some("  专属人设  ".to_string())
        );

        let mut blank = AppConfig::seeded();
        blank.system_prompt = "   ".to_string();
        assert_eq!(
            resolve_system_prompt(&blank, &request(vec![user("hi")])),
            None
        );
    }

    #[test]
    fn local_endpoints_may_skip_the_api_key() {
        let mut provider = ProviderConfig {
            id: "custom".to_string(),
            name: "自定义".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            models: vec![],
            is_preset: false,
        };
        assert!(is_keyless_provider(&provider));

        provider.base_url = "https://api.deepseek.com/v1".to_string();
        assert!(!is_keyless_provider(&provider));
    }

    #[test]
    fn cancelled_error_becomes_a_cancelled_event() {
        let event = terminal_event("r1", Err(AppError::new(code::CANCELLED, "已取消")));
        assert!(matches!(event, ChatEvent::Cancelled { .. }));
    }

    #[test]
    fn ordinary_error_becomes_an_error_event() {
        let event = terminal_event(
            "r1",
            Err(AppError::new(code::UNAUTHORIZED, "没配 Key").retryable(false)),
        );
        match event {
            ChatEvent::Error {
                code: c,
                retryable,
                message,
                ..
            } => {
                assert_eq!(c, code::UNAUTHORIZED);
                assert!(!retryable);
                assert_eq!(message, "没配 Key");
            }
            other => panic!("expected error event, got {other:?}"),
        }
    }
}
