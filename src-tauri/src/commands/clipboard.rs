//! 复制文本到系统剪贴板。
//!
//! 为什么不让前端直接用 `navigator.clipboard`：那个 API 要求「安全上下文 + 用户
//! 手势」，而悬浮窗会被程序化 show/hide、还会因为失焦自动隐藏 —— 这类场景下它
//! 会**静默失败**（promise 被拒绝，或者更糟：什么都没发生）。
//! 走 Rust 的 `arboard` 只有一种失败模式（剪贴板被别的程序占住），而且失败能
//! 被如实报出来。复制功能最怕的就是「以为复制成功了，粘出来还是上一次的内容」。
//!
//! 注意这是 `async` 命令：Tauri 里非 async 的命令跑在**主线程**上，
//! 而 Windows 剪贴板在别的程序占着时会重试等待 —— 那会把界面一起卡住。
//! `async` 命令由异步运行时执行，天然避开这一点（代价是必须返回 `Result`）。

use crate::errors::{AppError, AppResult};

/// 单次复制的上限。
///
/// 一条聊天记录最多也就几百 KB，4 MiB 已经远超正常用法。设这个上限是为了让
/// 「误把整个文件塞进剪贴板」这类调用尽早失败，而不是让别的程序读剪贴板时卡住。
const MAX_CLIPBOARD_BYTES: usize = 4 * 1024 * 1024;

/// 上限必须装得下正常用法里最长的那种内容：工具结果会被截到 8000 字符，
/// 「复制这条消息」不能在这类消息上失败（按 4 字节/字符的 UTF-8 上界算）。
///
/// 写成**编译期断言**而不是 `#[test]`：两边都是常量，clippy 会直接指出
/// 「这个断言的值是恒定的」。放在这里既能守住不变量，也不产生一条无意义的测试。
const _: () = assert!(MAX_CLIPBOARD_BYTES >= 8_000 * 4);

/// 长度校验单独抽出来，这样它可以在**不碰剪贴板**的前提下被单测覆盖
/// （无头环境里拿不到剪贴板，混在一起测会得到一个假失败）。
fn validate_length(text: &str) -> AppResult<()> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(AppError::invalid(format!(
            "要复制的内容太长（{} 字节，上限 {MAX_CLIPBOARD_BYTES} 字节）",
            text.len()
        )));
    }
    Ok(())
}

/// 真正的写剪贴板动作，与命令共用同一条路径（这样测试覆盖的就是产品代码）。
fn write_clipboard(text: &str) -> AppResult<()> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| AppError::internal(format!("无法访问系统剪贴板：{e}")))?;
    clipboard
        .set_text(text)
        .map_err(|e| AppError::internal(format!("写入系统剪贴板失败：{e}")))
}

/// 允许复制空串：那是「清空剪贴板」这个合法意图，不该被当成错误。
/// 空内容的**按钮**由前端决定不渲染（见 `src/lib/clipboard.ts` 的 `canCopy`）。
#[tauri::command]
pub async fn copy_to_clipboard(text: String) -> AppResult<()> {
    validate_length(&text)?;
    write_clipboard(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::code;

    #[test]
    fn oversized_payload_is_rejected() {
        let huge = "a".repeat(MAX_CLIPBOARD_BYTES + 1);
        let err = validate_length(&huge).unwrap_err();
        assert_eq!(err.code, code::INVALID_INPUT);
        // 错误信息必须说清「多长、上限多少」，否则用户不知道该截到多短。
        assert!(err.message.contains("太长"), "{}", err.message);
        assert!(err.message.contains(&MAX_CLIPBOARD_BYTES.to_string()));
    }

    #[test]
    fn a_payload_at_the_limit_is_accepted() {
        let at_limit = "a".repeat(MAX_CLIPBOARD_BYTES);
        assert!(validate_length(&at_limit).is_ok());
    }

    #[test]
    fn empty_text_is_legal() {
        // 复制空串是「清空剪贴板」的合法意图，不该报错。
        assert!(validate_length("").is_ok());
    }

    /// 真去写一遍系统剪贴板再读回来 —— 这是「复制」这个功能唯一的真相来源。
    ///
    /// 两个刻意的设计：
    /// - **先把用户原来的剪贴板内容存下来，测完还原**：测试不该顺手把用户
    ///   正在用的东西弄丢（这个项目里「划词取词」也是同样的规矩）；
    /// - 拿不到剪贴板时（无桌面会话 / CI）**跳过而不是失败**：那不是代码错了，
    ///   是环境不具备。但要显式打印出来，免得悄悄变成一条永远不跑的测试。
    #[test]
    fn text_round_trips_through_the_real_clipboard() {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            eprintln!("跳过：当前环境拿不到系统剪贴板");
            return;
        };
        let previous = clipboard.get_text().ok();

        let payload = "浮语剪贴板往返测试 · fuyu clipboard round-trip";
        write_clipboard(payload).expect("写入剪贴板应当成功");
        assert_eq!(
            clipboard.get_text().expect("读回剪贴板应当成功"),
            payload,
            "写进去和读出来的必须一模一样"
        );

        // 还原用户原来的剪贴板，失败也不该让测试挂掉。
        if let Some(previous) = previous {
            let _ = clipboard.set_text(previous);
        }
    }
}
