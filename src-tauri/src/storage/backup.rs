//! 对话备份的**文件格式与校验**。
//!
//! 为什么单独一个模块：这份结构是**对外契约**（用户会把文件存起来、以后导回来，
//! 甚至可能手改），所以它必须有显式的标记与版本号，而不是直接把数据库的行
//! 序列化了事。有了版本号，将来改格式时才能给出「请升级应用」这种明确提示，
//! 而不是一个 JSON 解析错误。
//!
//! 写入 / 读取数据库的部分在 [`super::database`]（那里能拿到连接）。

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

/// 备份文件的标记。用来挡住「随便选了个 JSON 文件」的情况。
pub const BACKUP_APP: &str = "fuyu";
/// 当前支持的备份格式版本。
pub const BACKUP_VERSION: u32 = 1;
/// 导入时的体积上限（字节）。
///
/// 导入要把整个文件读进内存再解析，所以必须有上限 —— 否则「误选了一个几个 G
/// 的文件」会把内存吃光。带很多图片的会话确实可能很大，所以这个值给得比较宽。
pub const MAX_BACKUP_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    /// 固定为 `"fuyu"`。
    pub app: String,
    pub version: u32,
    pub exported_at: i64,
    pub conversations: Vec<BackupConversation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupConversation {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub model_key: String,
    #[serde(default)]
    pub role_id: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub messages: Vec<BackupMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupMessage {
    pub id: String,
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub reasoning: Option<String>,
    /// `ok` / `cancelled` / `error`。
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub error_json: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<i64>,
    #[serde(default)]
    pub output_tokens: Option<i64>,
    /// 图片的 data URL 数组（原样带走 —— 备份不该悄悄丢数据）。
    #[serde(default)]
    pub images_json: Option<String>,
    #[serde(default)]
    pub created_at: i64,
}

fn default_status() -> String {
    "ok".to_string()
}

/// 导入结果，回给界面显示。
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// 新导入的会话数。
    pub imported: usize,
    /// 因为 id 已存在而**跳过**的会话数。
    pub skipped: usize,
    /// 新导入的消息数。
    pub messages: usize,
}

/// 解析并校验一份导出文件。
///
/// 三类错误分开报，因为它们对应完全不同的处置方式：
/// - 不是 JSON / 不是浮语的备份 → 让用户重新选文件；
/// - 版本更新 → 让用户升级应用（不是文件的错）；
/// - 太大 → 说明原因，而不是把内存吃光。
pub fn parse(json: &str) -> AppResult<Backup> {
    if json.len() > MAX_BACKUP_BYTES {
        return Err(AppError::invalid(format!(
            "备份文件太大（{:.0} MB，上限 {} MB）",
            json.len() as f64 / 1_048_576.0,
            MAX_BACKUP_BYTES / 1_048_576
        )));
    }

    let backup: Backup = serde_json::from_str(json)
        .map_err(|e| AppError::invalid(format!("这不是浮语的备份文件（无法解析：{e}）")))?;

    if backup.app != BACKUP_APP {
        return Err(AppError::invalid(format!(
            "这不是浮语的备份文件（app = {}）",
            backup.app
        )));
    }
    if backup.version > BACKUP_VERSION {
        return Err(AppError::invalid(format!(
            "备份文件的格式版本（{}）比当前应用支持的（{BACKUP_VERSION}）更新，请先升级应用",
            backup.version
        )));
    }

    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> String {
        serde_json::json!({
            "app": "fuyu",
            "version": 1,
            "exportedAt": 1_791_000_000_000i64,
            "conversations": [{
                "id": "conv_1",
                "title": "测试",
                "providerId": "deepseek",
                "modelKey": "deepseek-chat",
                "createdAt": 1,
                "updatedAt": 2,
                "messages": [
                    { "id": "m1", "role": "user", "content": "你好", "createdAt": 1 },
                    { "id": "m2", "role": "assistant", "content": "你也好", "createdAt": 2 }
                ]
            }]
        })
        .to_string()
    }

    #[test]
    fn a_valid_backup_parses() {
        let b = parse(&minimal()).unwrap();
        assert_eq!(b.app, BACKUP_APP);
        assert_eq!(b.conversations.len(), 1);
        assert_eq!(b.conversations[0].messages.len(), 2);
        // 缺省字段要有合理默认值，不能因为老文件少个字段就整份拒掉
        assert_eq!(b.conversations[0].messages[0].status, "ok");
        assert!(!b.conversations[0].pinned);
        assert!(b.conversations[0].role_id.is_none());
    }

    #[test]
    fn a_missing_field_falls_back_instead_of_failing() {
        // 手改过的文件（少几个字段）应当仍然能导入 —— 只保留 id/role/content 也能用。
        let json = serde_json::json!({
            "app": "fuyu",
            "version": 1,
            "exportedAt": 0,
            "conversations": [{
                "id": "conv_x",
                "messages": [{ "id": "m1", "role": "user", "content": "只有必填字段" }]
            }]
        })
        .to_string();
        let b = parse(&json).unwrap();
        assert_eq!(b.conversations[0].title, "");
        assert_eq!(b.conversations[0].messages[0].content, "只有必填字段");
    }

    #[test]
    fn someone_elses_json_is_rejected_with_a_clear_message() {
        let err = parse(r#"{"app":"other-app","version":1,"exportedAt":0,"conversations":[]}"#)
            .unwrap_err();
        assert!(
            err.message.contains("不是浮语的备份文件"),
            "{}",
            err.message
        );
    }

    #[test]
    fn random_json_is_rejected() {
        let err = parse(r#"{"hello":"world"}"#).unwrap_err();
        assert!(
            err.message.contains("不是浮语的备份文件"),
            "{}",
            err.message
        );
        assert!(parse("这不是 JSON").is_err());
    }

    #[test]
    fn a_newer_format_tells_the_user_to_upgrade() {
        // 这条与「文件不对」区分开：升级应用就能解决，不该让用户反复换文件。
        let json = r#"{"app":"fuyu","version":99,"exportedAt":0,"conversations":[]}"#;
        let err = parse(json).unwrap_err();
        assert!(err.message.contains("升级"), "{}", err.message);
        assert!(err.message.contains("99"));
    }

    #[test]
    fn an_oversized_payload_is_rejected_before_parsing() {
        let huge = "x".repeat(MAX_BACKUP_BYTES + 1);
        let err = parse(&huge).unwrap_err();
        assert!(err.message.contains("太大"), "{}", err.message);
    }

    #[test]
    fn an_empty_backup_is_valid_and_imports_nothing() {
        // 导出时一条会话都没有是很正常的情况（新装的用户就会这样）。
        let b = parse(r#"{"app":"fuyu","version":1,"exportedAt":0,"conversations":[]}"#).unwrap();
        assert!(b.conversations.is_empty());
    }
}
