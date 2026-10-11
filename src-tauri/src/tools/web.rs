//! 联网工具：**SSRF 安全**的网页抓取 + HTML 转文本。
//!
//! 为什么这块要单独写一大段并反复校验：`fetch_webpage` 的参数是**模型给的**。
//! 模型可以（被提示词注入后更是会）让它去请求 `http://169.254.169.254/`
//! （云元数据，能直接换到临时凭据）、`http://127.0.0.1:11434/`（本机服务）、
//! 或者 `http://10.0.0.5/admin`（内网后台）。所以每一次请求都必须先过白名单：
//!
//! 1. **只允许 http / https**，且 URL 不得携带用户名密码；
//! 2. **DNS 解析出的每一个 IP 都必须是公网地址** —— 一个域名可以同时解析出
//!    公网和内网两个记录，只要有一个是内网就整体拒绝；
//! 3. **把主机名钉死在已校验的那个 IP 上**（`ClientBuilder::resolve`）。
//!    否则「先解析校验、再让 reqwest 自己解析一次」之间有时间窗，攻击者可以
//!    在第 1 步返回公网、第 2 步返回内网（DNS rebinding）；
//! 4. **重定向自己跟**，每一跳都重新走上面的流程 —— 让 reqwest 自动跟随的话，
//!    一个公网 URL 可以 302 到内网；
//! 5. 响应体**流式限长**，超时封顶，避免一个坏站点把内存吃光。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use serde_json::Value;
use url::Url;

use super::registry::ToolResult;

/// 抓取时使用的 User-Agent（有些站点对没有 UA 的请求直接 403）。
const USER_AGENT: &str = "Mozilla/5.0 (compatible; FloatAI/0.1; +https://localhost)";

/// 抓取限制。
#[derive(Debug, Clone)]
pub struct FetchLimits {
    /// 响应体的最大字节数，超出即截断。
    pub max_bytes: usize,
    /// 单跳请求超时。
    pub timeout: Duration,
    /// 最多跟随几次重定向。
    pub max_redirects: usize,
    /// **仅测试用**：允许访问回环 / 内网地址。
    ///
    /// 生产路径必须为 `false`；集成测试要打本地假服务端，只能靠它开个口子。
    pub allow_private_ips: bool,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024,
            timeout: Duration::from_secs(15),
            max_redirects: 5,
            allow_private_ips: false,
        }
    }
}

/// 抓取结果。
#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub url: String,
    /// 已转成纯文本的正文。
    pub text: String,
    /// 是否因为超出 `max_bytes` 被截断。
    pub truncated: bool,
}

/// 一次安全请求的原始结果（未经 HTML / JSON 解释）。
#[derive(Debug, Clone)]
pub struct SafeResponse {
    /// 最终 URL（跟随重定向之后）。
    pub url: String,
    pub body: Vec<u8>,
    pub truncated: bool,
}

// ------------------------------------------------------------------ IP 校验

/// 这个 IP 是否属于「不允许外部工具访问」的地址段？
///
/// 覆盖：回环、私有网段、链路本地（含云元数据 `169.254.169.254`）、
/// 运营商级 NAT、组播、保留段，以及**能封装内网 IPv4 的 IPv6 形式**
/// （IPv4-mapped / 6to4 / Teredo）—— 最后这几类是常见绕过点。
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()             // 10/8、172.16/12、192.168/16
        || ip.is_loopback()      // 127/8
        || ip.is_link_local()    // 169.254/16 —— 云元数据就藏在这
        || ip.is_broadcast()     // 255.255.255.255
        || ip.is_unspecified()   // 0.0.0.0
        || ip.is_documentation() // 192.0.2/24、198.51.100/24、203.0.113/24
        || ip.is_multicast()     // 224/4
        // 运营商级 NAT：100.64.0.0/10
        || (o[0] == 100 && (64..128).contains(&o[1]))
        // 0.0.0.0/8「本网络」与 240.0.0.0/4 保留段
        || o[0] == 0
        || o[0] >= 240
        // 192.0.0.0/24（IETF 协议专用）
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        // 198.18.0.0/15（基准测试保留）
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    // 用 `to_ipv4()` 而不是 `to_ipv4_mapped()`：前者同时覆盖
    // IPv4-mapped（`::ffff:a.b.c.d`）与 **IPv4-compatible**（`::a.b.c.d`）两种
    // 写法。后者在 Windows/Linux 上已经连不通，但「连不通」不该是安全边界 ——
    // 判断逻辑本身要完整，否则哪天栈实现变回去就是一个洞。
    if let Some(v4) = ip.to_ipv4() {
        return is_blocked_v4(v4);
    }

    let s = ip.segments();
    ip.is_loopback()          // ::1
        || ip.is_unspecified() // ::
        // fc00::/7 唯一本地地址
        || (s[0] & 0xfe00) == 0xfc00
        // fe80::/10 链路本地
        || (s[0] & 0xffc0) == 0xfe80
        // ff00::/8 组播
        || (s[0] & 0xff00) == 0xff00
        // 2002::/16（6to4）与 2001:0::/32（Teredo）可以封装内网 IPv4
        || s[0] == 0x2002
        || (s[0] == 0x2001 && s[1] == 0x0000)
}

// ------------------------------------------------------------------ URL 校验

/// 校验一个**用户/模型给的** URL 是否允许抓取。
///
/// `allow_private_ips` 是给集成测试留的口子（要打本地假站点）：生产路径
/// 一律传 `false`。它**只**放开「地址归谁」——协议白名单、禁止 URL 内嵌
/// 凭据这两条永远生效，因为那两件事和地址归属无关。
fn validate_url(raw: &str, allow_private_ips: bool) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL 不能为空".to_string());
    }

    let url = Url::parse(trimmed).map_err(|e| format!("URL 解析失败：{e}"))?;

    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "只允许 http / https 协议，拒绝 {other}（file:、data: 等会读到本机文件）"
            ))
        }
    }

    // `http://user:pass@host/` 这种既可能是凭据泄漏，也常被用来混淆解析器。
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL 不允许携带用户名 / 密码".to_string());
    }

    if allow_private_ips {
        return Ok(url);
    }

    // 注意用 `url.host()` 而不是 `host_str()`：IPv6 字面量在 `host_str()` 里
    // 是**带方括号**的（`[::1]`），拿它去 `parse::<IpAddr>()` 会失败，于是
    // `http://[::1]:8080/` 就能绕过整个内网检查。必须用类型化的 `Host` 枚举。
    match url.host().ok_or("URL 缺少主机名")? {
        url::Host::Ipv4(v4) => {
            if is_blocked_ip(IpAddr::V4(v4)) {
                return Err(format!("拒绝访问本机 / 内网地址：{v4}"));
            }
        }
        url::Host::Ipv6(v6) => {
            if is_blocked_ip(IpAddr::V6(v6)) {
                return Err(format!("拒绝访问本机 / 内网地址：{v6}"));
            }
        }
        url::Host::Domain(name) => {
            // 常见内网主机名：通常解析到内网，不必等 DNS 就该拒绝。
            let lower = name.to_ascii_lowercase();
            if lower == "localhost"
                || lower.ends_with(".localhost")
                || lower.ends_with(".local")
                || lower.ends_with(".internal")
                || lower.ends_with(".home.arpa")
            {
                return Err(format!("拒绝访问本机 / 内网主机名：{name}"));
            }
        }
    }

    Ok(url)
}

// ------------------------------------------------------------------ 抓取

/// 安全抓取一个网页并转成纯文本。
pub async fn fetch_page_text(
    raw_url: &str,
    limits: &FetchLimits,
    proxy: Option<&str>,
) -> Result<FetchedPage, String> {
    let response = safe_get(raw_url, limits, proxy).await?;
    let raw = String::from_utf8_lossy(&response.body);
    Ok(FetchedPage {
        url: response.url,
        text: html_to_text(&raw),
        truncated: response.truncated,
    })
}

/// SSRF 安全的 GET：逐跳校验 + 钉死 IP + 手动跟随重定向 + 限长流式读取。
///
/// 刻意**不接收共享的 `reqwest::Client`**：要钉死 IP 就得用
/// `ClientBuilder::resolve`，而它只能在构造时设置。所以每一跳都新建一个
/// 客户端 —— 一次最多几个，换来的是「校验过的 IP 和真正连的 IP 是同一个」。
///
/// 代价是**代理也得单独传进来**（`proxy`）：共享客户端上的代理配置在这里
/// 一点用都没有。漏了它就会出现「聊天能通、抓网页直连」这种半残状态。
pub async fn safe_get(
    raw_url: &str,
    limits: &FetchLimits,
    proxy: Option<&str>,
) -> Result<SafeResponse, String> {
    let mut current = validate_url(raw_url, limits.allow_private_ips)?;
    let mut truncated = false;

    for _hop in 0..=limits.max_redirects {
        let host = current.host_str().ok_or("URL 缺少主机名")?.to_string();
        let port = current
            .port_or_known_default()
            .ok_or_else(|| format!("无法确定 {host} 的端口"))?;

        // ---- 1) 解析 DNS，并要求**每一个**地址都是公网。
        //
        // 配了代理时「本机解析不出来」不算致命：真正建连的是代理，由它去解析。
        // 企业网络里「本机 DNS 不出网、只留一个 HTTP 代理」是很常见的形态，
        // 在这里直接报错会让联网工具在那种环境下**完全不可用**。
        // 但只要能解析出来，就仍然逐个校验 —— 那是我们能拿到的最强证据。
        let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host.as_str(), port)).await {
            Ok(iter) => iter.collect(),
            Err(_) if proxy.is_some() => Vec::new(),
            Err(e) => return Err(format!("DNS 解析失败：{e}")),
        };

        if !limits.allow_private_ips {
            // 一个域名可以既有公网记录又有内网记录，出现一个内网就整体拒绝。
            for candidate in &addrs {
                if is_blocked_ip(candidate.ip()) {
                    return Err(format!("域名解析到内网地址，已拒绝：{}", candidate.ip()));
                }
            }
        }

        // ---- 2) 构造这一跳的客户端。
        let mut hop = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // 重定向必须自己跟，否则 302 到内网就绕过了上面所有检查。
            .redirect(reqwest::redirect::Policy::none());

        // 把主机名钉死在已校验的地址上，防 DNS rebinding。
        // 只有真的解析出了地址才钉 —— 没解析出来时连接由代理发起，无从可钉。
        if let Some(pinned) = addrs.first().copied() {
            hop = hop.resolve(&host, pinned);
        }

        // 用户配了代理就跟着用。注意 `resolve` 与代理并存时代理仍然生效：
        // 钉死的是「代理要连的主机」，而连接由代理进程发起。
        if let Some(url) = proxy.map(str::trim).filter(|u| !u.is_empty()) {
            match reqwest::Proxy::all(url) {
                Ok(configured) => hop = hop.proxy(configured),
                Err(e) => eprintln!("代理地址无效，联网工具将直连：{e}"),
            }
        }

        let hop_client = hop.build().map_err(|e| format!("构造请求失败：{e}"))?;

        let response = tokio::time::timeout(
            limits.timeout,
            hop_client
                .get(current.clone())
                .header("User-Agent", USER_AGENT)
                .header(
                    "Accept",
                    "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5",
                )
                .send(),
        )
        .await
        .map_err(|_| format!("请求超时（{} 秒）", limits.timeout.as_secs()))?
        .map_err(|e| format!("请求失败：{e}"))?;

        // ---- 3) 重定向：自己跟，下一跳重新走全套校验。
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or("重定向响应缺少 Location")?;
            let next = current
                .join(location)
                .map_err(|e| format!("重定向目标无法解析：{e}"))?;
            current = validate_url(next.as_str(), limits.allow_private_ips)?;
            continue;
        }

        if !response.status().is_success() {
            return Err(format!("目标返回 HTTP {}", response.status().as_u16()));
        }

        // ---- 4) 流式限长读取，别让一个坏站点把内存吃光。
        let mut body: Vec<u8> = Vec::new();
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("读取响应失败：{e}"))?
        {
            let remaining = limits.max_bytes.saturating_sub(body.len());
            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        return Ok(SafeResponse {
            url: current.to_string(),
            body,
            truncated,
        });
    }

    Err(format!("重定向次数超过上限（{} 次）", limits.max_redirects))
}

// ------------------------------------------------------------------ HTML 转文本

/// 把 HTML 粗转成纯文本。
///
/// 刻意不引入 `scraper` / `html2text`：这里的目标是「让模型读得懂正文」，
/// 不是精确还原排版。够用就行，而且少一个解析库就少一片攻击面。
pub fn html_to_text(html: &str) -> String {
    let without_scripts = strip_element(html, "script");
    let without_styles = strip_element(&without_scripts, "style");
    let without_head = strip_element(&without_styles, "head");
    let without_comments = strip_comments(&without_head);

    let mut out = String::with_capacity(without_comments.len() / 2);
    let mut chars = without_comments.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }
        // 读一个标签，顺便判断它是不是块级标签（块级要换成换行）。
        let mut tag = String::new();
        for inner in chars.by_ref() {
            if inner == '>' {
                break;
            }
            tag.push(inner);
        }
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        if is_block_tag(&name) || name == "br" {
            out.push('\n');
        } else if name == "td" || name == "th" {
            out.push('\t');
        }
    }

    decode_entities(&out)
}

fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "aside"
            | "nav"
            | "ul"
            | "ol"
            | "li"
            | "table"
            | "tr"
            | "blockquote"
            | "pre"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "figure"
            | "form"
            | "hr"
    )
}

/// 删掉 `<tag ...> ... </tag>` 整段（含内容）。
///
/// 两处必须小心的地方（都曾经把整页正文吃光）：
///
/// 1. **必须校验标签边界。** 只做前缀匹配的话 `<head` 会命中 `<header>`，
///    于是 `</head` 又命中 `</header>`，把夹在中间的正文整段删掉 ——
///    而 `<header>` 在现代网页里几乎无处不在；
/// 2. **找不到闭合标签时不能丢掉余下内容。** HTML5 允许省略 `</head>`，
///    真遇到就只跳过那个开标签。但 `script` / `style` 不同：它们的内容
///    *本身就是*要去掉的东西，没有闭合标签时只能整段丢弃。
fn strip_element(html: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}");
    // `to_ascii_lowercase` 只动 ASCII，**不改变字节长度**，所以下标可以通用。
    let lower = html.to_ascii_lowercase();
    // `script` / `style` 的内容是代码，不是正文。
    let drop_rest_when_unclosed = matches!(tag, "script" | "style");

    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;

    while let Some(start_rel) = lower[cursor..].find(&open) {
        let start = cursor + start_rel;
        let after_name = start + open.len();

        // 标签名后面必须紧跟 `>`、`/` 或空白，才算真的是这个标签。
        let boundary_ok = match lower[after_name..].chars().next() {
            None | Some('>') | Some('/') => true,
            Some(c) => c.is_whitespace(),
        };
        if !boundary_ok {
            // 是 `<header>` 这类同前缀的标签：原样保留，从标签名后面继续找。
            out.push_str(&html[cursor..after_name]);
            cursor = after_name;
            continue;
        }

        out.push_str(&html[cursor..start]);

        // 连开标签都没闭合（内容被上游截断）：余下内容照原样保留。
        let Some(open_end_rel) = lower[start..].find('>') else {
            out.push_str(&html[start..]);
            return out;
        };
        let after_open = start + open_end_rel + 1;

        match lower[after_open..].find(&close) {
            Some(close_rel) => {
                let close_at = after_open + close_rel;
                // 闭合标签自己也可能被截断。
                cursor = match lower[close_at..].find('>') {
                    Some(end_rel) => close_at + end_rel + 1,
                    None => after_open,
                };
            }
            None if drop_rest_when_unclosed => {
                // 未闭合的 script / style：后面都是代码，丢掉。
                cursor = html.len();
            }
            None => {
                // 未闭合的 head 之类：只跳过开标签，正文照常输出。
                cursor = after_open;
            }
        }
    }
    out.push_str(&html[cursor.min(html.len())..]);
    out
}

fn strip_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// 解码常见 HTML 实体（够用即可）。
fn decode_entities(text: &str) -> String {
    let out = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…");

    // 数字实体：&#123; 与 &#x1F600;
    //
    // 一次前向扫描 + 新建字符串，而不是 `find` + `replace_range` 循环：
    // 后者每替换一个实体都要把尾部字节整体挪一次，遇到一整页 `&#65;`
    // （512 KiB 上限内能塞十万个）就会退化成 O(n²)，白占一个 tokio 工作线程
    // 好几秒 —— 而这段文本是**抓来的网页**，完全由别人控制。
    let mut decoded = String::with_capacity(out.len());
    let mut cursor = 0usize;
    while let Some(rel) = out[cursor..].find("&#") {
        let start = cursor + rel;
        // 只在附近找分号：实体最长不过 `#x10FFFF;`，避免在长文本上反复扫描。
        let window_end = (start + 12).min(out.len());
        let Some(end_rel) = out[start..window_end].find(';') else {
            // 不是合法实体：原样保留 `&#` 两个字，继续往后。
            decoded.push_str(&out[cursor..start + 2]);
            cursor = start + 2;
            continue;
        };
        let end = start + end_rel;
        let body = &out[start + 2..end];
        let code = if let Some(hex) = body.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()
        } else {
            body.parse::<u32>().ok()
        };

        decoded.push_str(&out[cursor..start]);
        // 解不出来就丢掉这个实体（而不是留下 `&#…;` 干扰阅读）。
        if let Some(ch) = code.and_then(char::from_u32) {
            decoded.push(ch);
        }
        cursor = end + 1;
    }
    decoded.push_str(&out[cursor..]);
    let out = decoded;

    // 收敛空白：行内多空格压成一个，连续空行压成一个。
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            // 只在已有内容后补一个空行分隔。
            if matches!(lines.last(), Some(last) if !last.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(collapsed);
        }
    }
    while matches!(lines.last(), Some(last) if last.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

// ------------------------------------------------------------------ 联网搜索

/// 一次搜索最多回给模型几条结果。
const SEARCH_RESULT_LIMIT: usize = 8;

/// 搜索结果里的一条。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 把查询拼成 SearXNG 的 JSON 搜索地址。
///
/// `endpoint` 由**用户在设置里填写**，不是模型给的 —— 所以它和
/// `fetch_webpage` 面对的威胁模型不同：自建 SearXNG 常常就在局域网里
/// （`http://192.168.1.10:8888`），因此这里按「用户信任的地址」处理。
pub fn build_search_url(endpoint: &str, query: &str) -> Result<Url, String> {
    let mut url = Url::parse(endpoint.trim()).map_err(|e| format!("搜索端点无法解析：{e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("搜索端点只支持 http / https，收到 {other}")),
    }

    // 端点可能自带路径（例如 https://example.com/searx）。
    let base = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{base}/search"));
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("format", "json")
        .append_pair("language", "zh-CN")
        .append_pair("safesearch", "1");

    Ok(url)
}

/// 解析 SearXNG `format=json` 的响应体。
pub fn parse_search_results(body: &[u8], limit: usize) -> Result<Vec<SearchHit>, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| format!("搜索返回的不是合法 JSON（端点是否支持 format=json？）：{e}"))?;

    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or("搜索返回里没有 results 字段")?;

    let hits = results
        .iter()
        .filter_map(|item| {
            let url = item.get("url").and_then(Value::as_str)?.trim();
            if url.is_empty() {
                return None;
            }
            Some(SearchHit {
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("（无标题）")
                    .trim()
                    .to_string(),
                url: url.to_string(),
                snippet: item
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            })
        })
        .take(limit)
        .collect();

    Ok(hits)
}

/// `web_search` 工具的执行体。
///
/// `endpoint` 为 `None` 表示用户没配置搜索服务；这时工具根本不会下发给
/// 模型（见 `registry::to_openai_tools`），走到这里只可能是旧请求。
pub async fn run_web_search(
    endpoint: Option<&str>,
    args: &Value,
    proxy: Option<&str>,
) -> ToolResult {
    let Some(endpoint) = endpoint.filter(|value| !value.trim().is_empty()) else {
        return ToolResult::error(
            "联网搜索还没配置：请到「设置 → 通用」填写一个 SearXNG 端点（需要开启 format=json）",
        );
    };

    let Some(query) = args.get("query").and_then(Value::as_str) else {
        return ToolResult::error("缺少 query 参数");
    };
    let query = query.trim();
    if query.is_empty() {
        return ToolResult::error("query 不能为空");
    }

    let url = match build_search_url(endpoint, query) {
        Ok(url) => url,
        Err(e) => return ToolResult::error(e),
    };

    // 端点来自用户配置，允许指向局域网里的自建实例。
    let limits = FetchLimits {
        allow_private_ips: true,
        timeout: Duration::from_secs(20),
        ..Default::default()
    };

    let response = match safe_get(url.as_str(), &limits, proxy).await {
        Ok(response) => response,
        Err(e) => return ToolResult::error(format!("搜索失败：{e}")),
    };

    match parse_search_results(&response.body, SEARCH_RESULT_LIMIT) {
        Ok(hits) if hits.is_empty() => ToolResult::ok(format!("搜索「{query}」没有找到结果。")),
        Ok(hits) => {
            let mut out = format!("搜索「{query}」找到 {} 条结果：", hits.len());
            for (index, hit) in hits.iter().enumerate() {
                out.push_str(&format!(
                    "\n\n{}. {}\n   {}\n   {}",
                    index + 1,
                    hit.title,
                    hit.url,
                    hit.snippet
                ));
            }
            out.push_str("\n\n如需细读某条，用 fetch_webpage 抓取它的 URL。");
            ToolResult::ok(out)
        }
        Err(e) => ToolResult::error(e),
    }
}

// ------------------------------------------------------------------ 工具入口

/// `fetch_webpage` 工具的执行体。
pub async fn run_fetch_webpage(args: &Value, proxy: Option<&str>) -> ToolResult {
    let Some(raw_url) = args.get("url").and_then(Value::as_str) else {
        return ToolResult::error("缺少 url 参数");
    };

    match fetch_page_text(raw_url, &FetchLimits::default(), proxy).await {
        Ok(page) => {
            let mut content = format!("来源：{}\n\n{}", page.url, page.text);
            if page.truncated {
                content.push_str("\n\n（内容过长，已截断）");
            }
            // 注意：这里**不**标记 `touched_local_data`。抓到的网页不是本地
            // 敏感数据，语义上不该算；真正的防线是「每次联网都要用户看着
            // URL 点头」。网页内容是**不可信输入**，注入风险由那次审批兜住。
            ToolResult::ok(content)
        }
        Err(e) => ToolResult::error(format!("抓取失败：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    /// 测试里统一走**严格模式**（不允许内网地址）—— 生产路径也是这样。
    fn validate_public_url(raw: &str) -> Result<Url, String> {
        validate_url(raw, false)
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn loopback_and_private_ranges_are_blocked() {
        for addr in [
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.1",
            "169.254.169.254", // 云元数据
            "0.0.0.0",
            "100.64.0.1", // CGNAT
            "192.0.0.1",  // IETF 协议段
            "198.18.0.1", // 基准测试段
            "224.0.0.1",  // 组播
            "255.255.255.255",
            "240.0.0.1",
        ] {
            assert!(is_blocked_ip(ip(addr)), "{addr} 应当被拒绝");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for addr in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34", // example.com
            "172.32.0.1",    // 刚好在 172.16/12 之外
            "172.15.0.1",    // 刚好在 172.16/12 之外
            "100.63.0.1",    // 刚好在 CGNAT 之外
            "2606:4700:4700::1111",
        ] {
            assert!(!is_blocked_ip(ip(addr)), "{addr} 应当被放行");
        }
    }

    #[test]
    fn ipv6_forms_that_wrap_private_ipv4_are_blocked() {
        // 这几个是经典的绕过点：直接看字符串「不像内网」，但底层就是内网。
        assert!(is_blocked_ip(ip("::ffff:127.0.0.1")), "IPv4-mapped 回环");
        assert!(is_blocked_ip(ip("::ffff:10.0.0.1")), "IPv4-mapped 内网");
        assert!(
            is_blocked_ip(ip("::ffff:169.254.169.254")),
            "IPv4-mapped 元数据"
        );
        assert!(is_blocked_ip(ip("::1")), "IPv6 回环");
        assert!(is_blocked_ip(ip("::")), "未指定地址");
        assert!(is_blocked_ip(ip("fc00::1")), "唯一本地地址");
        assert!(is_blocked_ip(ip("fd12:3456::1")), "唯一本地地址");
        assert!(is_blocked_ip(ip("fe80::1")), "链路本地");
        assert!(is_blocked_ip(ip("ff02::1")), "组播");
        assert!(is_blocked_ip(ip("2002:7f00:0001::1")), "6to4 封装回环");
        assert!(is_blocked_ip(ip("2001:0:1234::1")), "Teredo");
        // IPv4-compatible（`::a.b.c.d`）与 mapped 是两种写法，都要拦。
        assert!(is_blocked_ip(ip("::127.0.0.1")), "IPv4-compatible 回环");
        assert!(is_blocked_ip(ip("::10.0.0.1")), "IPv4-compatible 内网");
        assert!(
            is_blocked_ip(ip("::169.254.169.254")),
            "IPv4-compatible 元数据"
        );
    }

    #[test]
    fn only_http_and_https_schemes_pass() {
        assert!(validate_public_url("https://example.com/a").is_ok());
        assert!(validate_public_url("http://example.com").is_ok());
        assert!(validate_public_url("  https://example.com  ").is_ok());

        for bad in [
            "file:///C:/Windows/System32/drivers/etc/hosts",
            "ftp://example.com/x",
            "data:text/html,<h1>hi</h1>",
            "gopher://example.com",
            "javascript:alert(1)",
        ] {
            let err = validate_public_url(bad).unwrap_err();
            assert!(
                err.contains("只允许 http / https"),
                "{bad} 应当被协议白名单挡下，实际：{err}"
            );
        }
    }

    #[test]
    fn credentials_in_the_url_are_rejected() {
        let err = validate_public_url("https://user:pass@example.com/").unwrap_err();
        assert!(err.contains("用户名"), "{err}");
    }

    #[test]
    fn literal_private_ips_and_local_hostnames_are_rejected() {
        for bad in [
            "http://127.0.0.1:11434/v1/models",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/admin",
            // 方括号 IPv6 是重点：`host_str()` 会带方括号，类型化 Host 才不会。
            "http://[::1]:8080/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fd00::1]/",
            "http://localhost:3000/",
            "http://foo.localhost/",
            "http://redis.internal/",
            "http://printer.local/",
            "http://metadata.google.internal/",
        ] {
            let err = validate_public_url(bad).unwrap_err();
            assert!(
                err.contains("拒绝访问"),
                "{bad} 应当被内网规则挡下，实际：{err}"
            );
        }
    }

    #[test]
    fn html_is_reduced_to_readable_text() {
        let html = r#"
            <html><head><title>页面小标题</title><style>body{color:red}</style></head>
            <body>
              <!-- 注释 -->
              <h1>大标题</h1>
              <p>第一段 &amp; 转义</p>
              <script>alert('xss')</script>
              <p>第二段</p>
              <ul><li>条目一</li><li>条目二</li></ul>
            </body></html>
        "#;

        let text = html_to_text(html);

        assert!(text.contains("大标题"));
        assert!(text.contains("第一段 & 转义"), "实体要解码：{text}");
        assert!(text.contains("第二段"));
        assert!(text.contains("条目一"));
        // 脚本 / 样式 / 注释 / head 里的 title 都不该出现。
        assert!(!text.contains("alert"), "script 内容必须删掉：{text}");
        assert!(!text.contains("color:red"), "style 内容必须删掉：{text}");
        assert!(!text.contains("注释"));
        assert!(
            !text.contains("页面小标题"),
            "head 里的 title 应当整段删掉：{text}"
        );
    }

    #[test]
    fn numeric_entities_are_decoded_without_hanging() {
        assert_eq!(html_to_text("<p>&#65;&#x42;</p>"), "AB");
        // 解不出来的实体要被丢掉，而不是留下 &# 让循环卡死。
        assert_eq!(html_to_text("<p>&#xZZ;</p>"), "");
        // 没有分号的 `&#` 是普通文本，不能吞掉后面的内容。
        assert_eq!(html_to_text("<p>a&#b</p>"), "a&#b");
    }

    #[test]
    fn a_page_of_numeric_entities_is_decoded_correctly() {
        // 顺带守住「单次前向扫描」这个实现：旧写法每替换一个实体就把尾部
        // 整体挪一次，这种输入会让它退化成 O(n²)。
        let html = format!("<p>{}</p>", "&#65;".repeat(5_000));
        let text = html_to_text(&html);
        assert_eq!(text.len(), 5_000);
        assert!(text.chars().all(|c| c == 'A'));
    }

    #[test]
    fn header_is_not_mistaken_for_head() {
        // `<head` 前缀匹配会命中 `<header>`，把夹在中间的正文整段删掉。
        let html = "<html><head><title>页面小标题</title></head>\
                    <body><header><h1>Rust 1.0 发布</h1></header>\
                    <p>正文段落</p></body></html>";

        let text = html_to_text(html);

        assert!(
            text.contains("Rust 1.0 发布"),
            "header 里的正文不能被当成 head 删掉：{text}"
        );
        assert!(text.contains("正文段落"));
        assert!(
            !text.contains("页面小标题"),
            "head 里的 title 仍然要删掉：{text}"
        );
    }

    #[test]
    fn a_missing_close_tag_does_not_discard_the_rest_of_the_page() {
        // HTML5 允许省略 </head>。旧实现在这里会把整个 body 丢掉，
        // 模型只拿到「来源：URL」，然后对着空白编答案。
        let html = "<html><head><title>T</title><body><p>重要正文</p></body></html>";
        let text = html_to_text(html);
        assert!(
            text.contains("重要正文"),
            "省略 </head> 不能把正文一起丢掉：{text:?}"
        );
    }

    #[test]
    fn an_unterminated_script_still_drops_its_code() {
        // 与 head 相反：脚本没闭合时，后面的内容**就是**代码，必须丢弃。
        let html = "<p>前面</p><script>var secret = 42;";
        let text = html_to_text(html);
        assert!(text.contains("前面"));
        assert!(
            !text.contains("secret"),
            "未闭合的脚本内容不能泄漏成正文：{text}"
        );
    }

    #[test]
    fn whitespace_is_collapsed() {
        let text = html_to_text("<p>a    b</p>\n\n\n\n<p>c</p>");
        // 行内多空格压成一个；段落之间保留一个空行（读起来还像文章）。
        assert_eq!(text, "a b\n\nc");
    }

    // ------------------------------------------------------- 抓取链路（本地假站点）

    /// 起一个按路径返回不同响应的本地服务端。
    ///
    /// 返回 `(base_url, 已请求的路径)`。测试里会把 `allow_private_ips` 打开，
    /// 否则连 127.0.0.1 都进不去 —— 这正是那个开关存在的唯一理由。
    async fn mock_site() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = paths.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                if read == 0 {
                    continue;
                }
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                // 请求行是 `GET /path?query HTTP/1.1`：**必须去掉查询串**，
                // 否则 `/search?q=…` 匹配不上 `/search` 分支。
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                recorder.lock().unwrap().push(path.clone());

                let (status, extra, body): (u16, &str, String) = match path.as_str() {
                    "/redirect" => (302, "Location: /page\r\n", String::new()),
                    "/loop" => (302, "Location: /loop\r\n", String::new()),
                    "/missing" => (404, "", "not found".to_string()),
                    "/big" => (200, "", "x".repeat(4096)),
                    "/search" => (
                        200,
                        "",
                        // 一条缺 url 的结果应当被跳过。
                        r#"{"results":[
                             {"title":"Rust 官网","url":"https://www.rust-lang.org/","content":"系统编程语言"},
                             {"title":"没有网址的结果","content":"应当被跳过"},
                             {"title":"官方文档","url":"https://doc.rust-lang.org/","content":"标准库文档"}
                           ]}"#
                        .to_string(),
                    ),
                    _ => (
                        200,
                        "",
                        "<html><body><h1>你好</h1><p>正文内容</p>\
                         <script>bad()</script></body></html>"
                            .to_string(),
                    ),
                };

                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            }
        });

        (format!("http://127.0.0.1:{}", addr.port()), paths)
    }

    fn test_limits() -> FetchLimits {
        FetchLimits {
            // 仅测试：本地假站点就是回环地址。
            allow_private_ips: true,
            max_bytes: 256 * 1024,
            timeout: Duration::from_secs(5),
            max_redirects: 3,
        }
    }

    /// 一个只回固定内容的假「HTTP 代理」，返回 (代理地址, 收到的请求行)。
    ///
    /// 用 `.invalid` 这种**解析不出来**的域名去访问，能一次性验证两件事：
    /// 1. 请求确实被送到了代理（否则它收不到连接）；
    /// 2. 本机 DNS 解析失败时，配了代理仍然能工作 —— 企业网络里
    ///    「本机不出网、只留一个代理」正是这个形态。
    async fn mock_proxy() -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();

            let body = "<html><body><p>经由代理取到的正文</p></body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
            request
        });

        (format!("http://127.0.0.1:{}", addr.port()), handle)
    }

    #[tokio::test]
    async fn the_configured_proxy_is_actually_used_by_the_web_tools() {
        // 这条测试守的是一个**很容易漏**的接线错误：联网工具刻意不复用共享的
        // `reqwest::Client`（要逐跳钉死 IP），所以代理必须单独传进来。
        // 漏传的话，用户会看到「聊天能通、抓网页和搜索全失败」。
        let (proxy, handle) = mock_proxy().await;

        let page = fetch_page_text(
            "http://does-not-exist.invalid/page",
            &test_limits(),
            Some(&proxy),
        )
        .await
        .expect("配了代理时，应当经由代理抓取成功");

        assert!(page.text.contains("经由代理取到的正文"), "{}", page.text);

        let request = handle.await.unwrap();
        // 走代理的请求行是**绝对形式**的，这是「真的经过代理」的直接证据。
        assert!(
            request.starts_with("GET http://does-not-exist.invalid/page"),
            "代理收到的请求行不对：{request}"
        );
    }

    #[tokio::test]
    async fn without_a_proxy_an_unresolvable_host_still_fails() {
        // 反向确认：不加代理时，解析不出来就该报错，而不是被上面的宽松分支放过。
        let err = fetch_page_text("http://does-not-exist.invalid/page", &test_limits(), None)
            .await
            .expect_err("没有代理时应当失败");
        assert!(err.contains("DNS"), "{err}");
    }

    #[tokio::test]
    async fn fetch_follows_redirects_and_strips_html() {
        let (base, paths) = mock_site().await;
        let page = fetch_page_text(&format!("{base}/redirect"), &test_limits(), None)
            .await
            .expect("应当抓取成功");

        assert!(page.text.contains("你好"), "{}", page.text);
        assert!(page.text.contains("正文内容"));
        assert!(!page.text.contains("bad()"), "script 必须被删掉");
        assert!(page.url.ends_with("/page"), "最终 URL 应当是重定向目标");

        // 逐跳都真的请求了 —— 证明重定向是自己跟的，而不是内核/客户端自动跳的。
        let seen = paths.lock().unwrap().clone();
        assert_eq!(seen, vec!["/redirect", "/page"], "应当逐跳请求");
    }

    #[tokio::test]
    async fn oversized_bodies_are_truncated_not_buffered_whole() {
        let (base, _paths) = mock_site().await;
        let limits = FetchLimits {
            max_bytes: 100,
            ..test_limits()
        };

        let page = fetch_page_text(&format!("{base}/big"), &limits, None)
            .await
            .expect("应当抓取成功");

        assert!(page.truncated, "超出上限必须标记为已截断");
        assert_eq!(page.text.len(), 100, "只应保留上限内的字节");
    }

    #[tokio::test]
    async fn non_success_statuses_are_reported() {
        let (base, _paths) = mock_site().await;
        let err = fetch_page_text(&format!("{base}/missing"), &test_limits(), None)
            .await
            .unwrap_err();
        assert!(err.contains("404"), "{err}");
    }

    #[tokio::test]
    async fn redirect_loops_give_up() {
        let (base, _paths) = mock_site().await;
        let err = fetch_page_text(&format!("{base}/loop"), &test_limits(), None)
            .await
            .unwrap_err();
        assert!(err.contains("重定向次数超过上限"), "{err}");
    }

    #[tokio::test]
    async fn the_private_ip_escape_hatch_does_not_loosen_scheme_checks() {
        // `allow_private_ips` 只为本地测试放行地址，协议白名单不受它影响。
        let limits = FetchLimits {
            allow_private_ips: true,
            ..test_limits()
        };
        let err = fetch_page_text("file:///C:/Windows/win.ini", &limits, None)
            .await
            .unwrap_err();
        assert!(err.contains("只允许 http"), "{err}");
    }

    // ------------------------------------------------------------ 联网搜索

    #[test]
    fn search_url_targets_the_json_endpoint() {
        let url = build_search_url("https://searx.example", "rust 所有权").unwrap();
        assert_eq!(url.path(), "/search");
        let query = url.query().unwrap();
        assert!(query.contains("format=json"), "{query}");
        assert!(query.contains("q=rust"), "{query}");

        // 端点自带路径时要接上，而不是把它冲掉。
        let nested = build_search_url("https://example.com/searx/", "a").unwrap();
        assert_eq!(nested.path(), "/searx/search");

        // 非 http(s) 端点直接拒绝。
        assert!(build_search_url("file:///etc/passwd", "a").is_err());
    }

    #[test]
    fn search_results_are_parsed_and_incomplete_entries_skipped() {
        // 注意用 `r#"..."#`（UTF-8 字符串）而不是 `br#"..."#`：
        // 原始**字节**串不允许非 ASCII 字符。
        let body = r#"{"results":[
            {"title":"A","url":"https://a.example","content":"aa"},
            {"title":"没有网址"},
            {"title":"B","url":"https://b.example"}
        ]}"#
        .as_bytes();

        let hits = parse_search_results(body, 10).unwrap();
        assert_eq!(hits.len(), 2, "缺 url 的结果要跳过");
        assert_eq!(hits[0].title, "A");
        assert_eq!(hits[0].url, "https://a.example");
        assert_eq!(hits[0].snippet, "aa");
        // 没有 content 的结果仍然保留，摘要为空串。
        assert_eq!(hits[1].snippet, "");

        // limit 生效。
        assert_eq!(parse_search_results(body, 1).unwrap().len(), 1);
    }

    #[test]
    fn malformed_search_responses_are_reported_not_panicking() {
        assert!(parse_search_results(b"not json", 5)
            .unwrap_err()
            .contains("JSON"));
        assert!(parse_search_results(br#"{"foo":1}"#, 5)
            .unwrap_err()
            .contains("results"));
        // 空结果集是合法的（只是没搜到）。
        assert!(parse_search_results(br#"{"results":[]}"#, 5)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn web_search_formats_results_for_the_model() {
        let (base, paths) = mock_site().await;

        let result = run_web_search(Some(&base), &json!({ "query": "rust" }), None).await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("Rust 官网"), "{}", result.content);
        assert!(result.content.contains("https://www.rust-lang.org/"));
        assert!(result.content.contains("系统编程语言"));
        // 缺 url 的那条不该出现在结果里。
        assert!(!result.content.contains("没有网址的结果"));
        // 提示模型下一步可以用 fetch_webpage 细读。
        assert!(result.content.contains("fetch_webpage"));

        assert_eq!(paths.lock().unwrap().as_slice(), ["/search"]);
    }

    #[tokio::test]
    async fn web_search_without_a_configured_endpoint_says_so() {
        let result = run_web_search(None, &json!({ "query": "rust" }), None).await;
        assert!(result.is_error);
        assert!(result.content.contains("设置"), "{}", result.content);

        // 空串等同于没配置。
        let blank = run_web_search(Some("   "), &json!({ "query": "rust" }), None).await;
        assert!(blank.is_error);

        // 缺 query。
        let missing = run_web_search(Some("https://searx.example"), &json!({}), None).await;
        assert!(missing.is_error);
        assert!(missing.content.contains("query"));
    }
}
