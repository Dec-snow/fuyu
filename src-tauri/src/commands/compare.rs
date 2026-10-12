//! 一问多答（模型对比）的后端。
//!
//! 对比本身**不需要新命令**：它复用 `chat_send`，只是：
//! - `conversationId: null` → 后端在没有会话时本来就不落库
//!   （落库那几处都是 `if let Some(conversation_id)`），
//!   于是对比天然是「草稿纸」，不会污染聊天历史；
//! - `enableTools: false` → 一路请求可能弹工具审批，N 路就会同时弹 N 个
//!   审批框，而对比面板并没有处理审批的界面。
//!
//! 这里只放一个「采用」命令：把选中的那一问一答写进会话。

use serde::Serialize;
use tauri::State;

use crate::errors::{AppError, AppResult};
use crate::storage::database::Database;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedTurn {
    pub user_message_id: String,
    pub assistant_message_id: String,
}

/// 把对比里的一问一答「采用」进会话。
///
/// 为什么连**问题**一起写：对比面板里的问题不在会话历史里，只写回答的话，
/// 会话里会出现一条没有提问的回答 —— 用户往后翻会看得莫名其妙。
///
/// 为什么带上服务商与模型：写进去之后它看起来就是一条正常的回答，
/// 而界面上会显示它是哪个模型答的（与其他回答一致）。
#[tauri::command]
pub async fn adopt_answer(
    db: State<'_, Database>,
    conversation_id: String,
    question: String,
    answer: String,
    provider_id: String,
    model_key: String,
) -> AppResult<AdoptedTurn> {
    let question = question.trim();
    let answer = answer.trim();
    if answer.is_empty() {
        return Err(AppError::invalid("这一路没有内容，没什么可采用的"));
    }
    if db.get_conversation(&conversation_id)?.is_none() {
        return Err(AppError::invalid("会话不存在了（可能已被删除）"));
    }

    // 问题为空时只写回答：有些对比是拿「同一个问题」反复试的，
    // 而问题可能已经被用户删掉了。不硬造一条空提问。
    let user_message_id = if question.is_empty() {
        String::new()
    } else {
        db.insert_message(
            &conversation_id,
            "user",
            question,
            &[],
            None,
            None,
            None,
            None,
            None,
        )?
    };
    let assistant_message_id = db.insert_message(
        &conversation_id,
        "assistant",
        answer,
        &[],
        None,
        Some(&provider_id),
        Some(&model_key),
        None,
        None,
    )?;
    // 会话列表按更新时间排序，写进新内容就该冒到最上面
    db.touch_conversation_only(&conversation_id)?;

    Ok(AdoptedTurn {
        user_message_id,
        assistant_message_id,
    })
}
