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

        // **环境能力探测**：剪贴板是全局共享资源，别的进程（浏览器、截图工具、
        // 远程桌面）可能正持有它，这时读写都会失败。所以先探一下能不能用 ——
        // 不能用就跳过，能用就下面用**硬断言**，两头都不含糊。
        //
        // 这条规则是踩出来的：跑完无头浏览器截图之后，这条测试开始报
        // `ContentNotAvailable`，看起来像产品 bug；而同时用 PowerShell 直接
        // 读写剪贴板也一样失败 —— 是环境被占住了，不是代码坏了。
        if clipboard
            .set_text("fuyu-clipboard-capability-probe")
            .is_err()
            || clipboard.get_text().is_err()
        {
            eprintln!("跳过：当前环境读写不了系统剪贴板（通常是有别的进程正占着它）");
            if let Some(previous) = previous {
                let _ = clipboard.set_text(previous);
            }
            return;
        }

        let payload = "浮语剪贴板往返测试 · fuyu clipboard round-trip";
        write_clipboard(payload).expect("写入剪贴板应当成功");

        // 读回来要**重试几次**（剪贴板的占用是瞬时的），并且区分两种情况：
        //
        // - 读到了内容但**不是**写入的那份 → 这是**真 bug**（产品的写入路径
        //   没生效），必须让测试红；
        // - 一次都没读到（全是 `ContentNotAvailable`）→ 环境不可用，跳过。
        //
        // 为什么不能简单地「失败就跳过」：那等于把测试的牙齿拔掉 ——
        // 写入路径要是真坏了，也会被当成环境问题糊过去。
        // 也不能「失败就红」：这台机器上有个卡死的 SystemSettings 占着剪贴板
        // （PowerShell 自己读写也失败），会让一条与环境无关的测试随机变红。
        let mut saw_any_read = false;
        let mut mismatch: Option<String> = None;
        for _ in 0..6 {
            match clipboard.get_text() {
                Ok(text) => {
                    saw_any_read = true;
                    if text == payload {
                        mismatch = None;
                        break;
                    }
                    mismatch = Some(text);
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(40)),
            }
        }

        if let Some(got) = mismatch {
            panic!("写进去和读出来的必须一模一样：写入 {payload:?}，读回 {got:?}");
        }
        if !saw_any_read {
            eprintln!("跳过：剪贴板当前不可读（通常是有别的进程正占着它）");
            if let Some(previous) = previous {
                let _ = clipboard.set_text(previous);
            }
            return;
        }

        // 还原用户原来的剪贴板，失败也不该让测试挂掉。
        if let Some(previous) = previous {
            let _ = clipboard.set_text(previous);
        }
    }
}
