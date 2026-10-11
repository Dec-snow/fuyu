//! 历史会话命令。
//!
//! 全部 `async`：Tauri 里非 async 的命令跑在**主线程**上，而这里每条命令都要
//! 访问 SQLite（`search_messages` 在短词回退时还可能对整库做一次 `LIKE` 全表扫）。
//! 放主线程会直接卡住界面渲染。
//!
//! 更彻底的做法是把这些同步的 `rusqlite` 调用丢进 `spawn_blocking`。目前单连接、
//! 单用户的规模下 `async fn` 已经足够，也不必为 `State<'_, Database>` 的
//! 生命周期绕路。

use serde::Serialize;
use tauri::State;

use crate::errors::{AppError, AppResult};
use crate::models::chat::{Conversation, MessageHit, StoredMessage};
use crate::storage::database::Database;
use crate::storage::new_id;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationDetail {
    pub conversation: Conversation,
    pub messages: Vec<StoredMessage>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[tauri::command]
pub async fn list_conversations(
    db: State<'_, Database>,
    limit: Option<i64>,
) -> AppResult<Vec<Conversation>> {
    db.list_conversations(limit.unwrap_or(200).clamp(1, 1000))
}

#[tauri::command]
pub async fn get_conversation(
    db: State<'_, Database>,
    id: String,
) -> AppResult<ConversationDetail> {
    let conversation = db
        .get_conversation(&id)?
        .ok_or_else(|| AppError::invalid("会话不存在"))?;
    let messages = db.list_messages(&id)?;
    Ok(ConversationDetail {
        conversation,
        messages,
    })
}

#[tauri::command]
pub async fn create_conversation(
    db: State<'_, Database>,
    provider_id: String,
    model_key: String,
    title: Option<String>,
) -> AppResult<Conversation> {
    db.create_conversation(&provider_id, &model_key, title.as_deref())
}

#[tauri::command]
pub async fn rename_conversation(
    db: State<'_, Database>,
    id: String,
    title: String,
) -> AppResult<()> {
    db.rename_conversation(&id, &title)
}

#[tauri::command]
pub async fn set_conversation_pinned(
    db: State<'_, Database>,
    id: String,
    pinned: bool,
) -> AppResult<()> {
    db.set_conversation_pinned(&id, pinned)
}

/// 给会话指定人设（`None` = 回落到全局系统提示词）。
///
/// 只让后端存 id，不在这里校验它是否存在：角色列表是配置的一部分，
/// 用户可能先删角色再改会话 —— 那样会话会退化成「用全局提示词」，
/// 而不是报错、把用户卡在一个改不动的状态里。解析交给发送时的
/// `resolveRolePrompt`（找不到就当没设）。
#[tauri::command]
pub async fn set_conversation_role(
    db: State<'_, Database>,
    id: String,
    role_id: Option<String>,
) -> AppResult<()> {
    db.set_conversation_role(&id, role_id.as_deref())
}

#[tauri::command]
pub async fn delete_conversation(db: State<'_, Database>, id: String) -> AppResult<()> {
    db.delete_conversation(&id)
}

#[tauri::command]
pub async fn clear_all_history(db: State<'_, Database>) -> AppResult<()> {
    db.clear_all_history()
}

/// 单条消息最多几张图。
pub const MAX_IMAGES_PER_MESSAGE: usize = 4;
/// 单张图片 data URL 的最大字符数（约 2.2 MB 二进制；base64 会膨胀约 1/3）。
pub const MAX_IMAGE_DATA_URL_CHARS: usize = 3_000_000;

/// 允许的图片 data URL 前缀。
///
/// **刻意白名单，而不是黑名单**：`data:text/html` 能执行脚本，
/// `data:image/svg+xml` 里也能塞 `<script>` —— 而图片最后是要塞进
/// WebView 的 `<img src>` 里渲染的。只放行真正的位图格式，
/// 这两种就都进不来。
const ALLOWED_IMAGE_PREFIXES: &[&str] = &[
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/jpg;base64,",
    "data:image/webp;base64,",
    "data:image/gif;base64,",
];

/// 这个 data URL 是不是允许的位图？
pub fn is_supported_image_data_url(url: &str) -> bool {
    ALLOWED_IMAGE_PREFIXES
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

/// 校验一批图片；返回错误说明第几张不合格。
pub fn validate_images(images: &[String]) -> AppResult<()> {
    if images.len() > MAX_IMAGES_PER_MESSAGE {
        return Err(AppError::invalid(format!(
            "一条消息最多 {MAX_IMAGES_PER_MESSAGE} 张图片，收到 {} 张",
            images.len()
        )));
    }
    for (index, url) in images.iter().enumerate() {
        if !is_supported_image_data_url(url) {
            return Err(AppError::invalid(format!(
                "第 {} 张图片格式不支持（只接受 png / jpeg / webp / gif）",
                index + 1
            )));
        }
        if url.len() > MAX_IMAGE_DATA_URL_CHARS {
            return Err(AppError::invalid(format!(
                "第 {} 张图片太大（上限约 {} MB）",
                index + 1,
                MAX_IMAGE_DATA_URL_CHARS / 1_000_000
            )));
        }
    }
    Ok(())
}

/// 落一条用户消息，返回它的 id。
///
/// 用户消息由前端**先**落库、再调 `chat_send`，这样即使网络请求失败，
/// 用户说的话也不会丢。图片也一样：先落库，之后 `chat_send` 只回传消息 id，
/// 由后端从库里取出图片 —— 免得每次提问都在 IPC 上搬几 MB 的 base64。
#[tauri::command]
pub async fn save_user_message(
    db: State<'_, Database>,
    conversation_id: String,
    content: String,
    images: Option<Vec<String>>,
) -> AppResult<String> {
    let images = images.unwrap_or_default();
    validate_images(&images)?;

    // 只发图片、不写字是合法的（「这张图里是什么？」的直接形态）。
    if content.trim().is_empty() && images.is_empty() {
        return Err(AppError::invalid("消息内容为空"));
    }

    let id = db.insert_message(
        &conversation_id,
        "user",
        &content,
        &images,
        None,
        None,
        None,
        None,
        None,
    )?;
    // 让会话冒到列表顶部。失败不影响消息本身，只记日志。
    if let Err(e) = db.touch_conversation_only(&conversation_id) {
        eprintln!("更新会话时间失败：{e}");
    }
    Ok(id)
}

#[tauri::command]
pub async fn update_message(db: State<'_, Database>, id: String, content: String) -> AppResult<()> {
    if content.trim().is_empty() {
        return Err(AppError::invalid("消息内容为空"));
    }
    db.update_message_content(&id, &content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bitmap_data_urls_are_accepted() {
        for ok in [
            "data:image/png;base64,AAAA",
            "data:image/jpeg;base64,AAAA",
            "data:image/jpg;base64,AAAA",
            "data:image/webp;base64,AAAA",
            "data:image/gif;base64,AAAA",
        ] {
            assert!(is_supported_image_data_url(ok), "{ok} 应当被接受");
        }

        // 这几种都是**真实的注入面**：`text/html` 能执行脚本，
        // `image/svg+xml` 里也能塞 `<script>` —— 而它们最终要进 WebView 的
        // `<img src>` 渲染。白名单必须只放行真正的位图。
        for bad in [
            "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
            "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=",
            "data:image/svg+xml,<svg onload=alert(1)>",
            "javascript:alert(1)",
            "https://evil.example/a.png",
            "data:application/pdf;base64,AAAA",
            "data:image/png,notbase64",
            "",
        ] {
            assert!(!is_supported_image_data_url(bad), "{bad} 必须被拒绝");
        }
    }

    #[test]
    fn too_many_or_too_large_images_are_rejected() {
        let png = "data:image/png;base64,AAAA".to_string();

        assert!(validate_images(&[]).is_ok());
        assert!(validate_images(&vec![png.clone(); MAX_IMAGES_PER_MESSAGE]).is_ok());

        let too_many = vec![png.clone(); MAX_IMAGES_PER_MESSAGE + 1];
        let err = validate_images(&too_many).unwrap_err();
        assert!(err.message.contains("最多"), "{}", err.message);

        let huge = format!(
            "data:image/png;base64,{}",
            "A".repeat(MAX_IMAGE_DATA_URL_CHARS)
        );
        let err = validate_images(&[huge]).unwrap_err();
        assert!(err.message.contains("太大"), "{}", err.message);
    }

    #[test]
    fn the_error_says_which_image_is_bad() {
        let images = vec![
            "data:image/png;base64,AAAA".to_string(),
            "data:text/html;base64,AAAA".to_string(),
        ];
        let err = validate_images(&images).unwrap_err();
        assert!(err.message.contains("第 2 张"), "{}", err.message);
    }
}

#[tauri::command]
pub async fn delete_message(db: State<'_, Database>, id: String) -> AppResult<()> {
    db.delete_message(&id)
}

/// 「编辑后重跑」：取该消息之前的历史，供前端重新发起请求。
#[tauri::command]
pub async fn messages_before(
    db: State<'_, Database>,
    conversation_id: String,
    message_id: String,
) -> AppResult<Vec<StoredMessage>> {
    db.messages_before(&conversation_id, &message_id)
}

#[tauri::command]
pub async fn search_messages(
    db: State<'_, Database>,
    query: String,
    limit: Option<i64>,
) -> AppResult<Vec<MessageHit>> {
    db.search_messages(&query, limit.unwrap_or(30).clamp(1, 200))
}

#[tauri::command]
pub async fn token_totals(db: State<'_, Database>) -> AppResult<UsageTotals> {
    let (input_tokens, output_tokens) = db.token_totals()?;
    Ok(UsageTotals {
        input_tokens,
        output_tokens,
    })
}

/// 生成一个新的会话 id（前端「新对话」时先占位用）。
///
/// 纯计算、无 IO，保持同步 —— 省掉一次运行时调度。
#[tauri::command]
pub fn new_conversation_id() -> String {
    new_id("conv_")
}
