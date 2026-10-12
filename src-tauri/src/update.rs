//! 检查更新所需的纯逻辑：版本号比较与 GitHub Release 响应解析。
//!
//! ## 为什么不做「静默下载并自动安装」
//!
//! Tauri 官方的 `tauri-plugin-updater` 能做真正的自动更新，但它要求**签名**：
//! 更新包要用私钥签名、应用内置公钥校验，否则任何能改掉下载源的人都能让应用
//! 静默运行一段任意代码。在 Windows 上这还牵涉代码签名证书。
//!
//! 这个项目没有签名证书，也不该在没有签名的情况下提供「自动下载并运行」——
//! 那等于把「用户信任这个应用」变成「用户信任应用访问的每一个环节」。
//! 所以这里只做**检查 + 告知 + 打开下载页**，最后一步由用户自己走。
//!
//! （另一个现实原因：这台开发机上 `static.crates.io` 解析不了，
//! `tauri-plugin-updater` 这个依赖根本装不进来。但即使能装，上面那条理由依然成立。）

use serde::Serialize;

use crate::errors::{code, AppError, AppResult};

/// 发布说明的长度上限（Release 正文可能很长，界面放不下）。
pub const MAX_NOTES_CHARS: usize = 4000;

/// 一个解析好的版本号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// 预发布标识（`1.2.3-beta.1` 里的 `beta.1`）。
    ///
    /// **有预发布标识的版本小于同号的正式版** —— 这是 semver 的规则，
    /// 也是「发了个 beta 给用户试、正式版出来后要能提示升级」的前提。
    pub pre: Option<String>,
}

/// 解析版本号。接受 `1.2.3` / `v1.2.3` / `1.2` / `1.2.3-beta.1`。
///
/// 解析不了就返回 `None` 而不是猜一个：拿一个猜出来的版本号去比较，
/// 得出的「有新版本」是不可信的。
pub fn parse_version(raw: &str) -> Option<Version> {
    let text = raw.trim();
    let text = text
        .strip_prefix('v')
        .or_else(|| text.strip_prefix('V'))
        .unwrap_or(text);
    if text.is_empty() {
        return None;
    }

    // 预发布：第一个 `-` 之后全是 pre（`+build` 元数据直接丢掉，它不参与比较）
    let (core, pre) = match text.split_once('-') {
        Some((core, rest)) => {
            let rest = rest.split('+').next().unwrap_or(rest);
            (core, Some(rest.to_string()))
        }
        None => (text.split('+').next().unwrap_or(text), None),
    };

    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    // 缺的部分当 0：`1.2` 与 `1.2.0` 是同一个版本
    let minor = parts
        .next()
        .map(|s| s.parse::<u64>())
        .transpose()
        .ok()?
        .unwrap_or(0);
    let patch = parts
        .next()
        .map(|s| s.parse::<u64>())
        .transpose()
        .ok()?
        .unwrap_or(0);
    if parts.next().is_some() {
        return None;
    }

    Some(Version {
        major,
        minor,
        patch,
        pre,
    })
}

/// `candidate` 是不是比 `current` 新。
///
/// 比较不了（有一边解析不出来）时返回 `false` —— **宁可不提示，也不误报**。
/// 误报「有新版本」会让用户点进一个不存在的下载页，比不提示更糟。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(a), Some(b)) = (parse_version(candidate), parse_version(current)) else {
        return false;
    };
    compare(&a, &b) == std::cmp::Ordering::Greater
}

fn compare(a: &Version, b: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.major, a.minor, a.patch).cmp(&(b.major, b.minor, b.patch)) {
        Ordering::Equal => match (&a.pre, &b.pre) {
            // 都没有预发布标识 → 相等
            (None, None) => Ordering::Equal,
            // 有预发布标识的更小（1.0.0-beta < 1.0.0）
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(x), Some(y)) => compare_pre(x, y),
        },
        other => other,
    }
}

/// 预发布标识的比较（`beta.1` < `beta.2` < `rc.1`）。
///
/// 按「数字段按数值比、其它按字典序、数字段小于字母段」这个 semver 简化版来。
/// 够用了：真正的排序规则（点分段之间逐段比较）在这里并不值得；我们只需要
/// 「同一个版本号下，预发布小于正式」这一条一定是**确定**的。
fn compare_pre(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.split('.'), b.split('.'));
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    // 数字段小于字母段（semver：1.0.0-1 < 1.0.0-alpha）
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// 解析出来的一次发布。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInfo {
    pub tag: String,
    pub name: String,
    pub notes: String,
    pub page_url: String,
    pub published_at: String,
    /// 给 Windows 用的安装包（没有就为 `None`）。
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub asset_size: Option<u64>,
}

/// 解析 GitHub `/releases/latest` 的响应。
pub fn parse_release(body: &str) -> AppResult<ReleaseInfo> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::new(code::SERVER, format!("发布信息不是合法 JSON：{e}")))?;

    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if tag.is_empty() {
        return Err(AppError::new(code::SERVER, "发布信息里没有版本号"));
    }

    let page_url = value
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    // 没有下载页就没什么可给用户的 —— 宁可不提示
    if !is_safe_update_url(&page_url) {
        return Err(AppError::new(
            code::SERVER,
            "发布信息里的下载页地址不可用".to_string(),
        ));
    }

    let notes = truncate_notes(
        value
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim(),
    );

    // 挑安装包：Windows 上就是那个 .exe（`-setup.exe` 优先，那是 NSIS 安装器）
    let mut asset_name = None;
    let mut asset_url = None;
    let mut asset_size = None;
    if let Some(assets) = value.get("assets").and_then(serde_json::Value::as_array) {
        let pick = assets
            .iter()
            .filter_map(|asset| {
                let name = asset.get("name")?.as_str()?.to_string();
                let url = asset.get("browser_download_url")?.as_str()?.to_string();
                if !name.to_ascii_lowercase().ends_with(".exe") {
                    return None;
                }
                let size = asset.get("size").and_then(serde_json::Value::as_u64);
                // setup 优先，其次任意 exe
                let rank = if name.to_ascii_lowercase().contains("setup") {
                    0
                } else {
                    1
                };
                Some((rank, name, url, size))
            })
            .min_by_key(|(rank, name, _, _)| (*rank, name.clone()));
        if let Some((_, name, url, size)) = pick {
            asset_name = Some(name);
            asset_url = Some(url);
            asset_size = size;
        }
    }

    Ok(ReleaseInfo {
        tag,
        name: value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        notes,
        page_url,
        published_at: value
            .get("published_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        asset_name,
        asset_url,
        asset_size,
    })
}

/// 只允许打开 GitHub 的 https 地址。
///
/// 「打开外部链接」这个动作把 URL 交给了操作系统，所以**不能**照单全收服务端
/// 返回的任何字符串：一份被篡改的响应可以塞进 `file://` 或自定义协议。
/// 这里限制得比「是 https」更死一点 —— 必须是 github.com 域下的地址。
pub fn is_safe_update_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://github.com/")
        || lower.starts_with("https://objects.githubusercontent.com/")
}

fn truncate_notes(text: &str) -> String {
    if text.chars().count() <= MAX_NOTES_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_NOTES_CHARS).collect();
    out.push_str("\n\n…（说明太长，已截断，完整内容见发布页）");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_parsed_in_the_shapes_people_write_them() {
        assert_eq!(
            parse_version("1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3,
                pre: None
            })
        );
        assert_eq!(
            parse_version("v0.1.0"),
            Some(Version {
                major: 0,
                minor: 1,
                patch: 0,
                pre: None
            })
        );
        // 缺段当 0：`1.2` 与 `1.2.0` 是同一个版本
        assert_eq!(parse_version("1.2"), parse_version("1.2.0"));
        assert_eq!(
            parse_version("1.2.3-beta.1"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3,
                pre: Some("beta.1".to_string())
            })
        );
        // 构建元数据不参与比较
        assert_eq!(parse_version("1.2.3+build.7"), parse_version("1.2.3"));
    }

    #[test]
    fn unparseable_versions_are_reported_as_none() {
        for bad in ["", "v", "abc", "1.2.3.4", "1.x.3", "-1.2.3", "1.2.3-"] {
            // `1.2.3-` 的 pre 是空串，仍然解析得出来；其余应当失败
            if bad == "1.2.3-" {
                assert!(parse_version(bad).is_some());
                continue;
            }
            assert_eq!(parse_version(bad), None, "{bad} 不该解析成功");
        }
    }

    #[test]
    fn newer_versions_are_recognized() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(is_newer("v0.2.0", "0.1.0"));
        // 同样新
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
        // 更旧
        assert!(!is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn a_prerelease_is_older_than_the_release_it_leads_to() {
        // 发了个 beta 给用户试，正式版出来后要能提示升级
        assert!(is_newer("1.0.0", "1.0.0-beta.1"));
        assert!(!is_newer("1.0.0-beta.1", "1.0.0"));
        // 同一个正式版下的两个 beta
        assert!(is_newer("1.0.0-beta.2", "1.0.0-beta.1"));
        assert!(is_newer("1.0.0-rc.1", "1.0.0-beta.9"));
    }

    #[test]
    fn an_unparseable_side_never_reports_an_update() {
        // 宁可不提示，也不误报：误报会让用户点进一个不存在的下载页
        assert!(!is_newer("abc", "1.0.0"));
        assert!(!is_newer("1.0.0", "abc"));
        assert!(!is_newer("", ""));
    }

    #[test]
    fn a_release_is_parsed_with_its_installer() {
        // 注意用 r###"…"### ：正文里有 `"##`（JSON 的引号紧跟 Markdown 标题），
        // 而 r##"…"## 的终止符正是 `"##` —— 会在那里被**提前结束**，整个文件都编译不过。
        // 每加一层 `#`，终止符就多一个 `#`，所以三层足够躲开这种内容。
        let body = r###"{
            "tag_name": "v0.2.0",
            "name": "0.2.0 — 本地知识库",
            "body": "## 新东西\n- 知识库\n- 长期记忆",
            "html_url": "https://github.com/Dec-snow/fuyu/releases/tag/v0.2.0",
            "published_at": "2025-01-01T00:00:00Z",
            "assets": [
                {"name": "fuyu_0.2.0_x64-setup.exe", "browser_download_url": "https://github.com/Dec-snow/fuyu/releases/download/v0.2.0/fuyu_0.2.0_x64-setup.exe", "size": 12345678},
                {"name": "source.tar.gz", "browser_download_url": "https://github.com/x/y.tar.gz", "size": 10}
            ]
        }"###;
        let release = parse_release(body).unwrap();
        assert_eq!(release.tag, "v0.2.0");
        assert!(release.notes.contains("知识库"));
        assert_eq!(
            release.asset_name.as_deref(),
            Some("fuyu_0.2.0_x64-setup.exe")
        );
        assert_eq!(release.asset_size, Some(12345678));
    }

    #[test]
    fn the_setup_installer_wins_over_other_executables() {
        let body = r#"{
            "tag_name": "v1.0.0",
            "html_url": "https://github.com/Dec-snow/fuyu/releases/tag/v1.0.0",
            "assets": [
                {"name": "portable.exe", "browser_download_url": "https://github.com/a/portable.exe", "size": 1},
                {"name": "fuyu_1.0.0_x64-setup.exe", "browser_download_url": "https://github.com/a/setup.exe", "size": 2}
            ]
        }"#;
        let release = parse_release(body).unwrap();
        assert_eq!(
            release.asset_name.as_deref(),
            Some("fuyu_1.0.0_x64-setup.exe")
        );
    }

    #[test]
    fn a_release_without_an_installer_is_still_usable() {
        let body = r#"{
            "tag_name": "v1.0.0",
            "html_url": "https://github.com/Dec-snow/fuyu/releases/tag/v1.0.0",
            "assets": []
        }"#;
        let release = parse_release(body).unwrap();
        assert!(release.asset_name.is_none());
        // 仍然可以打开下载页
        assert!(is_safe_update_url(&release.page_url));
    }

    #[test]
    fn unusable_releases_are_rejected() {
        for body in [
            "not json",
            r#"{}"#,
            r#"{"tag_name":""}"#,
            // 没有 html_url 就没什么可给用户的
            r#"{"tag_name":"v1.0.0"}"#,
            // 下载页不是 github：不能交给系统去打开
            r#"{"tag_name":"v1.0.0","html_url":"file:///C:/Windows/System32/calc.exe"}"#,
            r#"{"tag_name":"v1.0.0","html_url":"javascript:alert(1)"}"#,
        ] {
            assert!(parse_release(body).is_err(), "{body} 应当被拒");
        }
    }

    #[test]
    fn only_github_https_urls_are_openable() {
        assert!(is_safe_update_url(
            "https://github.com/Dec-snow/fuyu/releases/tag/v1.0.0"
        ));
        for bad in [
            "http://github.com/x",           // 不是 https
            "https://evil.com/github.com/x", // 域不对
            "https://github.com.evil.com/x", // 后缀伪装
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!is_safe_update_url(bad), "{bad} 不该放行");
        }
    }

    #[test]
    fn long_notes_are_truncated_with_a_pointer_to_the_release_page() {
        let long = "啊".repeat(MAX_NOTES_CHARS + 100);
        let body = format!(
            r#"{{"tag_name":"v1.0.0","html_url":"https://github.com/a/b","body":"{long}"}}"#
        );
        let release = parse_release(&body).unwrap();
        assert!(release.notes.chars().count() < long.chars().count());
        assert!(release.notes.contains("发布页"));
    }
}
