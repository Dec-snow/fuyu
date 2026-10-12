//! 检查更新。
//!
//! 只做三件事：问 GitHub「最新发布是哪个版本」→ 和当前版本比 → 告诉界面。
//! **不下载、不安装**，理由见 `crate::update` 的文件头（没有代码签名的情况下做
//! 静默自动安装，等于把「用户信任这个应用」换成「用户信任应用访问的每一个环节」）。

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::errors::{code, AppError, AppResult};
use crate::state::AppState;
use crate::storage::config::ConfigStore;
use crate::update::{self, ReleaseInfo};

/// 两次自动检查之间的间隔。
///
/// 不是随手定的：GitHub 对**未鉴权**请求限制每小时 60 次，而每次打开设置页、
/// 每次启动都可能来问一次。六小时一次既够及时，也不会把配额烧掉。
const CHECK_INTERVAL_SECS: i64 = 6 * 60 * 60;

/// 请求 GitHub 时必须带的 User-Agent（不带会被拒）。
const USER_AGENT: &str = "fuyu-update-check";

/// 上一次检查的结果（进程内缓存，重启就没了 —— 这正是想要的）。
static CACHE: Mutex<Option<UpdateCheck>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    /// 当前版本（来自 Cargo.toml）。
    pub current: String,
    /// 最新发布的版本号；`None` = 还没有发布过任何版本。
    pub latest: Option<String>,
    pub has_update: bool,
    /// 用户主动跳过了这个版本。
    pub skipped: bool,
    pub notes: String,
    pub page_url: String,
    pub published_at: String,
    pub asset_name: Option<String>,
    pub asset_size: Option<u64>,
    pub checked_at: i64,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tauri::command]
pub async fn check_update(
    state: State<'_, AppState>,
    config: State<'_, ConfigStore>,
    force: bool,
) -> AppResult<UpdateCheck> {
    let (repo, skipped) = config.read(|cfg| {
        (
            cfg.update_repo.trim().to_string(),
            cfg.skipped_version.trim().to_string(),
        )
    });
    let current = env!("CARGO_PKG_VERSION").to_string();

    // 缓存：非强制时六小时内直接给上次的结果
    if !force {
        if let Some(cached) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            if now_secs() - cached.checked_at < CHECK_INTERVAL_SECS {
                return Ok(cached);
            }
        }
    }

    if repo.is_empty() || !repo.contains('/') {
        return Err(AppError::invalid(
            "更新仓库地址要写成「用户名/仓库名」，例如 Dec-snow/fuyu",
        ));
    }

    let release = match fetch_latest(&state.client(), &repo).await? {
        Some(release) => release,
        // 这个仓库还没有发布过任何 release。这不是错误，是「暂时没有新版」。
        None => {
            let result = UpdateCheck {
                current,
                latest: None,
                has_update: false,
                skipped: false,
                notes: String::new(),
                page_url: String::new(),
                published_at: String::new(),
                asset_name: None,
                asset_size: None,
                checked_at: now_secs(),
            };
            *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
            return Ok(result);
        }
    };

    let has_update = update::is_newer(&release.tag, &current);
    let result = UpdateCheck {
        current,
        latest: Some(release.tag.clone()),
        has_update,
        skipped: has_update && release.tag == skipped,
        notes: release.notes,
        page_url: release.page_url,
        published_at: release.published_at,
        asset_name: release.asset_name,
        asset_size: release.asset_size,
        checked_at: now_secs(),
    };
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
    Ok(result)
}

/// 问 GitHub 要最新发布。`None` 表示这个仓库还没有发布过（404）。
///
/// 抽出来是为了能测：命令本身要 `State<AppState>`，在测试里构造不出来，
/// 而「真的能连上 GitHub 并解析出发布信息」恰恰是最值得亲手验一次的部分。
async fn fetch_latest(client: &reqwest::Client, repo: &str) -> AppResult<Option<ReleaseInfo>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let response = client
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| AppError::network(format!("连不上 GitHub：{e}")))?;

    let status = response.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if status == 403 || status == 429 {
        return Err(AppError::new(
            code::RATE_LIMITED,
            "GitHub 拒绝了这次检查（多半是请求太频繁），过一会儿再试".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(AppError::new(
            code::SERVER,
            format!("检查更新失败（HTTP {status}）"),
        ));
    }

    let body = response.text().await.unwrap_or_default();
    Ok(Some(update::parse_release(&body)?))
}

/// 用系统默认浏览器打开发布页。
///
/// 地址**必须**先过 [`update::is_safe_update_url`]：这个动作把 URL 交给了
/// 操作系统，而 URL 来自网络响应 —— 一份被篡改的响应可以塞进 `file://` 或
/// 自定义协议，那就是「点一下检查更新，运行了本地某个程序」。
#[tauri::command]
pub async fn open_update_page(url: String) -> AppResult<()> {
    if !update::is_safe_update_url(&url) {
        return Err(AppError::invalid(
            "这个地址不是 GitHub 上的发布页，拒绝打开",
        ));
    }
    open_in_browser(&url)
}

#[cfg(windows)]
fn open_in_browser(url: &str) -> AppResult<()> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let operation = HSTRING::from("open");
    let target = HSTRING::from(url);
    // 用 ShellExecuteW 而不是 `cmd /C start`：后者会把 URL 交给命令行解释器，
    // URL 里的 `&` / `^` 之类的字符就有了别的含义。
    let result = unsafe { ShellExecuteW(None, &operation, &target, None, None, SW_SHOWNORMAL) };
    // ShellExecuteW 的约定：返回值 <= 32 表示失败（它不是错误码，是个 HINSTANCE）
    if result.0 as isize <= 32 {
        return Err(AppError::new(
            code::SERVER,
            "打不开浏览器，可以手动复制发布页地址".to_string(),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_in_browser(_url: &str) -> AppResult<()> {
    Err(AppError::invalid("当前平台不支持自动打开浏览器"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_interval_respects_githubs_unauthenticated_quota() {
        // 未鉴权请求每小时 60 次；六小时一次 = 一天最多 4 次，余量足够。
        // （第一版写成 `* 4 < 24h` 是错的：6h × 4 正好等于 24h，边界不成立，
        //   而且那是个常量表达式，clippy 会直接拒。）
        assert_eq!(CHECK_INTERVAL_SECS, 21600);
        let checks_per_day = 24 * 60 * 60 / CHECK_INTERVAL_SECS;
        assert!(
            (1..=6).contains(&checks_per_day),
            "一天查 {checks_per_day} 次太频繁或太少"
        );
    }

    #[test]
    fn only_github_pages_can_be_opened() {
        // 打开外链这个动作把 URL 交给了操作系统，所以必须有白名单式的校验
        assert!(update::is_safe_update_url(
            "https://github.com/Dec-snow/fuyu/releases/tag/v1.0.0"
        ));
        assert!(!update::is_safe_update_url(
            "file:///C:/Windows/System32/calc.exe"
        ));
    }

    #[test]
    fn the_current_version_comes_from_the_package_manifest() {
        // 版本号只有一个来源，避免「设置里显示的版本」和「比较用的版本」不一致
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
        assert!(update::parse_version(env!("CARGO_PKG_VERSION")).is_some());
    }

    /// 真的连一次 GitHub。
    ///
    /// 标成 `#[ignore]`：整个测试套件是**离线**的，这条要显式跑：
    /// `cargo test --offline -- --ignored live_github --nocapture`
    ///
    /// 判据写得宽是有意的：这个仓库现在**还没有任何 release**（应当拿到 `Ok(None)`），
    /// 但以后发了版本就该走另一条分支 —— 两种都算通过，失败只可能是「连不上」
    /// 或「响应解析不了」，而那两条才是这条测试要盯的东西。
    #[tokio::test]
    #[ignore = "需要联网：cargo test --offline -- --ignored live_github"]
    async fn live_github_lookup_works() {
        let client = reqwest::Client::new();
        match fetch_latest(&client, "Dec-snow/fuyu").await {
            Ok(None) => println!("仓库还没有发布任何 release（符合预期）"),
            Ok(Some(release)) => {
                println!("最新发布 = {} · {}", release.tag, release.name);
                assert!(update::parse_version(&release.tag).is_some());
                assert!(update::is_safe_update_url(&release.page_url));
                assert!(update::is_newer(&release.tag, "0.0.1"));
            }
            Err(e) => panic!("连不上 GitHub 或响应解析不了：{}", e.message),
        }
    }
}
