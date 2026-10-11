//! 对话备份的导出与导入。
//!
//! 格式与校验在 [`crate::storage::backup`]，这里只负责「文件放哪、怎么落盘」。

use std::path::{Path, PathBuf};

use tauri::State;

use crate::errors::{AppError, AppResult};
use crate::storage::backup::{self, ImportReport};
use crate::storage::config::ConfigStore;
use crate::storage::database::Database;

/// 导出结果：界面要告诉用户「存到哪了、有多大」。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub bytes: usize,
    pub conversations: usize,
    pub messages: usize,
}

/// 把备份写成文件，返回 `(路径, 字节数)`。
///
/// 抽成普通函数是为了能单测 —— 命令要 `State`，构造不出来，
/// 而「文件到底写没写、写到哪、目录不存在会不会失败」恰恰是这条链路上
/// 唯一没有被数据库层测试覆盖的部分。
fn write_export(base: &Path, json: &str, stamp: i64) -> AppResult<(PathBuf, usize)> {
    let dir = base.join(EXPORT_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("fuyu-export-{stamp}.json"));
    std::fs::write(&path, json.as_bytes())?;
    Ok((path, json.len()))
}

/// 导出目录名（ASCII，方便用户手敲路径）。
const EXPORT_DIR: &str = "exports";

/// 把全部历史导出成一个 JSON 文件，返回它的绝对路径。
///
/// 为什么写到固定的 `exports/` 目录，而不是弹一个「另存为」对话框：
/// 原生对话框要额外的插件与权限（`tauri-plugin-dialog`），而这一步的目标是
/// **先让备份真的能存下来**。文件名带时间戳，界面把完整路径显示出来并允许
/// 一键复制 —— 用户想挪到哪儿都行。
#[tauri::command]
pub async fn export_history(
    db: State<'_, Database>,
    config: State<'_, ConfigStore>,
) -> AppResult<ExportResult> {
    let data = db.export_backup()?;
    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| AppError::internal(format!("序列化备份失败：{e}")))?;

    let base = config
        .path()
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| AppError::internal("配置文件没有所在目录，无法确定导出位置"))?;
    let (path, bytes) = write_export(&base, &json, crate::storage::now_ms())?;

    let messages = data.conversations.iter().map(|c| c.messages.len()).sum();
    Ok(ExportResult {
        path: path.display().to_string(),
        bytes,
        conversations: data.conversations.len(),
        messages,
    })
}

/// 从备份文件的内容导入（只新增，id 已存在的会话跳过）。
///
/// 内容由前端读文件后传进来（`<input type=file>`），而不是传路径：
/// WebView 里拿不到文件的真实路径，而为了一个导入功能引入
/// `tauri-plugin-fs` + 权限配置并不划算。体积上限由
/// [`backup::MAX_BACKUP_BYTES`] 把关。
#[tauri::command]
pub async fn import_history(db: State<'_, Database>, json: String) -> AppResult<ImportReport> {
    let data = backup::parse(&json)?;
    db.import_backup(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("floatai-export-{}", crate::storage::new_id("")));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_export_file_lands_in_the_exports_folder_and_round_trips() {
        let base = temp_dir();
        let json = r#"{"app":"fuyu","version":1,"exportedAt":0,"conversations":[]}"#;

        let (path, bytes) = write_export(&base, json, 1_791_000_000_000).unwrap();

        assert!(path.starts_with(base.join(EXPORT_DIR)), "必须写进 exports/");
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "fuyu-export-1791000000000.json"
        );
        assert_eq!(bytes, json.len(), "返回的字节数要和实际写入一致");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), json);

        // 写出来的东西必须还能被自己解析回来（否则「导出了但导不回去」）
        backup::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_missing_exports_folder_is_created() {
        // 首次导出时 `exports/` 还不存在 —— 这条路径必须自己建目录，
        // 否则用户点第一次「导出」只会得到一个错误。
        let base = temp_dir();
        assert!(!base.join(EXPORT_DIR).exists());
        write_export(&base, "{}", 1).unwrap();
        assert!(base.join(EXPORT_DIR).is_dir());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn two_exports_do_not_overwrite_each_other() {
        // 文件名带时间戳的意义就在这里：连点两次不会把前一次覆盖掉。
        let base = temp_dir();
        let (a, _) = write_export(&base, "{\"first\":true}", 1).unwrap();
        let (b, _) = write_export(&base, "{\"second\":true}", 2).unwrap();
        assert_ne!(a, b);
        assert!(a.exists() && b.exists());
        std::fs::remove_dir_all(&base).ok();
    }
}
