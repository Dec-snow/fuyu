//! MCP（Model Context Protocol）的**纯协议层**：JSON-RPC 帧、工具名映射、结果拍平。
//!
//! 与进程管理分开（那些在 [`super::client`]）：这一层不碰 IO，所以能单测 ——
//! 而它恰好是最容易出错的地方（工具名撞车、结果结构千奇百怪）。
//!
//! 传输只做 **stdio**：MCP 服务端绝大多数是本地进程（`npx -y @modelcontextprotocol/
//! server-filesystem /path`、`uvx mcp-server-git` 之类）。HTTP/SSE 传输没做，
//! 这是**明确的取舍**而不是遗漏 —— 见 README 的「未实现」列表。

use serde::Serialize;
use serde_json::{json, Value};

use crate::errors::{code, AppError, AppResult};

/// MCP 协议版本。服务端不接受时它会回自己支持的版本，我们以它为准（不挑三拣四）。
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// 工具名（含前缀）的长度上限。OpenAI 的 function name 上限是 64。
pub const MAX_TOOL_NAME: usize = 64;

/// 一个 MCP 服务器上报的工具。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// 服务端那边的**原始**工具名（调用时要用它）。
    pub raw_name: String,
    /// 暴露给模型的合格名字（`mcp__服务__工具`）。
    pub name: String,
    pub description: String,
    /// JSON Schema 形式的参数定义。
    pub parameters: Value,
}

/// 一个 MCP 服务器的连接与发现结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub id: String,
    pub name: String,
    /// 是否连上了（启动 + 握手成功）。
    pub connected: bool,
    /// 服务端自报的名字与版本。
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    /// 发现到的工具。
    pub tools: Vec<McpTool>,
    /// 失败原因（连不上时给人看）。
    pub error: Option<String>,
}

/// 把服务端工具名映射成模型能用的名字。
///
/// 三条规则：
/// - **加前缀**：`mcp__服务__工具`。这不是为了好看，而是为了**不与内置工具撞车** ——
///   一个 MCP 服务完全可能有一个叫 `calculator` 的工具，而内置的 calculator
///   已经是敏感/非敏感策略的一部分，撞上就是权限混乱；
/// - **只留 `[A-Za-z0-9_-]`**：OpenAI 的 function name 有字符集限制，带上 `.` `/` 的
///   工具名（很常见，例如 `fs.read_file`）会被直接拒；
/// - **截断到 64**：超长名字同样会被拒。截断时保留前缀与尾部哈希，避免两个长名字
///   被截成同一个（那样调用会打到错误的工具上）。
pub fn qualify_tool_name(server_id: &str, raw_tool: &str) -> String {
    let prefix = format!("mcp__{}__", sanitize(server_id));
    let body = sanitize(raw_tool);

    if prefix.len() + body.len() <= MAX_TOOL_NAME {
        return format!("{prefix}{body}");
    }

    // 超长：留一点空间给 `_` + 8 位哈希
    let hash = short_hash(&format!("{server_id}\u{0}{raw_tool}"));
    let keep = MAX_TOOL_NAME
        .saturating_sub(prefix.len())
        .saturating_sub(hash.len() + 1);
    let mut truncated: String = body.chars().take(keep).collect();
    truncated.push('_');
    truncated.push_str(&hash);
    format!("{prefix}{truncated}")
}

/// 从合格名字里拆回 `(server_id, raw_tool)`。
///
/// 只用于「这个名字是不是我们的工具」的判断与日志；真正的调用参数由
/// [`McpTool`] 一路带着走，不靠反解（反解在截断之后就不可靠了）。
pub fn split_tool_name(qualified: &str) -> Option<(&str, &str)> {
    let rest = qualified.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

/// 这个名字是不是 MCP 工具。
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with("mcp__")
}

fn sanitize(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// 一个短哈希（截断名字时用来避免撞车）。
///
/// 自己实现一个 FNV-1a 而不是引 md5/sha：这里只需要「不同输入几乎不会撞」，
/// 不需要密码学强度，也不值得多一个依赖。
fn short_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

/// 造一条 JSON-RPC 请求（一行一条，末尾换行 —— MCP 的 stdio 传输是按行分帧的）。
pub fn build_request(id: u64, method: &str, params: Value) -> String {
    let mut line = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
    })
    .to_string();
    // params 为 null 时不带这个字段（有的实现会挑）
    if !params.is_null() {
        let mut value: Value = serde_json::from_str(&line).unwrap_or(json!({}));
        value["params"] = params;
        line = value.to_string();
    }
    line.push('\n');
    line
}

/// 造一条通知（没有 id，服务端不会回）。
pub fn build_notification(method: &str, params: Value) -> String {
    let mut value = json!({ "jsonrpc": "2.0", "method": method });
    if !params.is_null() {
        value["params"] = params;
    }
    let mut line = value.to_string();
    line.push('\n');
    line
}

/// 解析一条来自服务端的消息。
#[derive(Debug, Clone, PartialEq)]
pub enum RpcMessage {
    /// 对某条请求的应答。
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<String>,
    },
    /// 服务端主动发的通知（例如日志）—— 忽略，但不能当成错误。
    Notification { method: String },
    /// 既没有 id 也没有 method：无法处理的垃圾行。
    Unknown,
}

pub fn parse_message(line: &str) -> Option<RpcMessage> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;

    if let Some(method) = value.get("method").and_then(Value::as_str) {
        // 有 id 的 method 是「服务端反过来请求我们」（sampling/roots 之类）。
        // 我们不支持，所以回一条「方法不支持」—— 但**不能**当通知静默丢掉，
        // 否则服务端会一直等。
        if value.get("id").is_some() {
            return Some(RpcMessage::Response {
                id: value.get("id").and_then(Value::as_u64).unwrap_or(0),
                result: None,
                error: Some(format!("不支持服务端请求：{method}")),
            });
        }
        return Some(RpcMessage::Notification {
            method: method.to_string(),
        });
    }

    let Some(id) = value.get("id").and_then(Value::as_u64) else {
        return Some(RpcMessage::Unknown);
    };
    let error = value.get("error").map(|e| {
        e.get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| e.to_string())
    });
    Some(RpcMessage::Response {
        id,
        result: value.get("result").cloned(),
        error,
    })
}

/// 从 `tools/list` 的结果里取出工具。
pub fn parse_tools_list(result: &Value, server_id: &str) -> Vec<McpTool> {
    let Some(items) = result.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let Some(raw_name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        if raw_name.trim().is_empty() {
            continue;
        }
        out.push(McpTool {
            raw_name: raw_name.to_string(),
            name: qualify_tool_name(server_id, raw_name),
            description: item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string(),
            // 缺参数定义时给一个空对象，而不是 `null`：
            // OpenAI 要求 parameters 是个合法的 JSON Schema。
            parameters: item
                .get("inputSchema")
                .cloned()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        });
    }
    out
}

/// 把 `tools/call` 的结果拍平成一段文本喂给模型。
///
/// MCP 的结果是「内容块数组」：`{ content: [{type:"text",text:"…"},
/// {type:"image",data:"…"}, {type:"resource",resource:{…}}], isError?: bool }`。
/// 模型这里只吃文本，所以：
/// - `text` 原样收下；
/// - `image` / `audio` **不塞 base64**（几 MB 的 base64 会瞬间吃掉上下文预算），
///   只说明「这里有一张图」；
/// - `resource` 取 uri 与文本内容。
///
/// `isError` 为真时把内容包成错误文本 —— 让模型知道「这个工具失败了」，
/// 而不是把错误信息当成正常结果继续编。
pub fn flatten_tool_result(result: &Value) -> String {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut parts: Vec<String> = Vec::new();
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                kind @ ("image" | "audio") => {
                    let mime = item.get("mimeType").and_then(Value::as_str).unwrap_or(kind);
                    parts.push(format!("（这里有一份 {mime} 内容，当前无法直接展示）"));
                }
                "resource" => {
                    let resource = item.get("resource").cloned().unwrap_or(json!({}));
                    let uri = resource
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or("未知位置");
                    match resource.get("text").and_then(Value::as_str) {
                        Some(text) => parts.push(format!("【{uri}】\n{text}")),
                        None => parts.push(format!("【{uri}】（二进制资源，未取回）")),
                    }
                }
                other => parts.push(format!("（未支持的内容类型：{other}）")),
            }
        }
    }

    if parts.is_empty() {
        parts.push("（工具没有返回内容）".to_string());
    }
    let joined = parts.join("\n\n");
    if is_error {
        format!("工具执行失败：{joined}")
    } else {
        joined
    }
}

/// 把握手结果里的服务端信息取出来。
pub fn parse_server_info(result: &Value) -> (Option<String>, Option<String>) {
    let info = result.get("serverInfo");
    (
        info.and_then(|i| i.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        info.and_then(|i| i.get("version"))
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// 握手请求的 params。
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "fuyu", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// 调用工具时的 params。
pub fn call_params(raw_name: &str, args: &Value) -> Value {
    json!({
        "name": raw_name,
        "arguments": if args.is_object() { args.clone() } else { json!({}) },
    })
}

/// 把一个 `tools/call` 的应答变成文本；服务端报错误时返回 `Err`。
pub fn tool_call_text(result: Option<&Value>, error: Option<&str>) -> AppResult<String> {
    if let Some(error) = error {
        return Err(AppError::new(
            code::SERVER,
            format!("MCP 服务端报错：{error}"),
        ));
    }
    let Some(result) = result else {
        return Err(AppError::new(code::SERVER, "MCP 服务端没有返回结果"));
    };
    Ok(flatten_tool_result(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_prefixed_and_sanitized() {
        // 常见形态：带点的工具名（fs.read_file）会被 OpenAI 拒，必须换成合法字符
        assert_eq!(
            qualify_tool_name("files", "read_file"),
            "mcp__files__read_file"
        );
        assert_eq!(
            qualify_tool_name("files", "fs.read_file"),
            "mcp__files__fs_read_file"
        );
        // 前缀保证了不会与内置工具撞车
        assert_ne!(qualify_tool_name("s", "calculator"), "calculator");
        assert!(is_mcp_tool(&qualify_tool_name("s", "calculator")));
    }

    #[test]
    fn a_name_that_is_too_long_is_truncated_without_colliding() {
        let long_a = "a".repeat(100);
        let long_b = format!("{}b", "a".repeat(99));
        let a = qualify_tool_name("server", &long_a);
        let b = qualify_tool_name("server", &long_b);

        assert!(a.len() <= MAX_TOOL_NAME, "{} 太长", a.len());
        assert!(b.len() <= MAX_TOOL_NAME);
        // 截断不能把两个不同的名字截成同一个 —— 那会调用到错误的工具
        assert_ne!(a, b, "长名字截断后撞车了");
    }

    #[test]
    fn names_with_odd_characters_do_not_produce_empty_or_illegal_names() {
        for raw in ["", "   ", "中文工具", "a/b\\c", "...", "🙂"] {
            let name = qualify_tool_name("srv", raw);
            assert!(!name.is_empty());
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{raw} → {name} 含非法字符"
            );
        }
    }

    #[test]
    fn splitting_a_qualified_name_works_and_rejects_the_rest() {
        assert_eq!(
            split_tool_name("mcp__files__read_file"),
            Some(("files", "read_file"))
        );
        for bad in ["calculator", "mcp__", "mcp__only", "mcp____x", "mcp__s__"] {
            assert_eq!(split_tool_name(bad), None, "{bad} 不该拆出东西");
        }
    }

    #[test]
    fn requests_are_one_line_json() {
        let line = build_request(1, "tools/list", json!({}));
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "必须是一行一条（按行分帧）");
        let value: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 1);
        assert_eq!(value["method"], "tools/list");
    }

    #[test]
    fn notifications_have_no_id() {
        let line = build_notification("notifications/initialized", json!({}));
        let value: Value = serde_json::from_str(line.trim()).unwrap();
        assert!(value.get("id").is_none());
        assert_eq!(value["method"], "notifications/initialized");
    }

    #[test]
    fn responses_are_parsed_by_id() {
        let msg = parse_message(r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#).unwrap();
        match msg {
            RpcMessage::Response { id, result, error } => {
                assert_eq!(id, 7);
                assert!(result.is_some());
                assert!(error.is_none());
            }
            other => panic!("应当是应答：{other:?}"),
        }

        let msg = parse_message(
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"没有这个方法"}}"#,
        )
        .unwrap();
        match msg {
            RpcMessage::Response { id, result, error } => {
                assert_eq!(id, 7);
                assert!(result.is_none());
                assert!(error.unwrap().contains("没有这个方法"));
            }
            other => panic!("应当是错误应答：{other:?}"),
        }
    }

    #[test]
    fn notifications_and_garbage_are_handled_without_panicking() {
        // 服务端会主动发日志通知，不能当成「不认识的应答」卡住
        assert_eq!(
            parse_message(r#"{"jsonrpc":"2.0","method":"notifications/message","params":{}}"#),
            Some(RpcMessage::Notification {
                method: "notifications/message".to_string()
            })
        );
        // 服务端反过来请求我们（sampling/roots）：必须回一条错误，不能静默丢弃
        match parse_message(r#"{"jsonrpc":"2.0","id":3,"method":"sampling/createMessage"}"#)
            .unwrap()
        {
            RpcMessage::Response { id, error, .. } => {
                assert_eq!(id, 3);
                assert!(error.unwrap().contains("不支持"));
            }
            other => panic!("应当回一条错误：{other:?}"),
        }
        assert_eq!(parse_message(""), None);
        assert_eq!(parse_message("   "), None);
        assert_eq!(parse_message("这不是 JSON"), None);
        assert_eq!(
            parse_message(r#"{"没有id也没有method":1}"#),
            Some(RpcMessage::Unknown)
        );
    }

    #[test]
    fn a_tools_list_is_parsed_into_usable_definitions() {
        let result = json!({
            "tools": [
                {
                    "name": "read_file",
                    "description": "读一个文件",
                    "inputSchema": { "type": "object", "properties": { "path": { "type": "string" } } }
                },
                // 缺 description 与 inputSchema 的也要能用
                { "name": "list_dir" },
                // 没有名字的直接跳过
                { "description": "没有名字" }
            ]
        });
        let tools = parse_tools_list(&result, "files");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "mcp__files__read_file");
        assert_eq!(tools[0].raw_name, "read_file");
        assert!(tools[0].parameters["properties"]["path"].is_object());
        // 缺 inputSchema 时给一个合法的空 schema，而不是 null
        assert_eq!(tools[1].parameters["type"], "object");
    }

    #[test]
    fn an_empty_or_broken_tools_list_yields_nothing_instead_of_panicking() {
        assert!(parse_tools_list(&json!({}), "s").is_empty());
        assert!(parse_tools_list(&json!({ "tools": "不是数组" }), "s").is_empty());
        assert!(parse_tools_list(&json!({ "tools": [] }), "s").is_empty());
    }

    #[test]
    fn tool_results_are_flattened_to_text() {
        let result = json!({
            "content": [
                { "type": "text", "text": "第一段" },
                { "type": "text", "text": "第二段" }
            ]
        });
        let text = flatten_tool_result(&result);
        assert!(text.contains("第一段") && text.contains("第二段"));

        // 图片不塞 base64（会瞬间吃掉上下文预算），只说明有图
        let with_image = json!({
            "content": [
                { "type": "text", "text": "看这张图" },
                { "type": "image", "mimeType": "image/png", "data": "AAAA".repeat(1000) }
            ]
        });
        let text = flatten_tool_result(&with_image);
        assert!(text.contains("看这张图"));
        assert!(text.contains("image/png"));
        assert!(!text.contains("AAAAAAAA"), "不能把 base64 塞进上下文");
    }

    #[test]
    fn resources_are_shown_with_their_location() {
        let result = json!({
            "content": [
                { "type": "resource", "resource": { "uri": "file:///tmp/a.txt", "text": "内容" } },
                { "type": "resource", "resource": { "uri": "file:///tmp/b.bin" } }
            ]
        });
        let text = flatten_tool_result(&result);
        assert!(text.contains("file:///tmp/a.txt"));
        assert!(text.contains("内容"));
        assert!(text.contains("file:///tmp/b.bin"));
    }

    #[test]
    fn an_error_result_is_marked_as_an_error_for_the_model() {
        // 不标出来的话，模型会把错误信息当成正常结果继续编
        let result = json!({
            "isError": true,
            "content": [{ "type": "text", "text": "文件不存在" }]
        });
        let text = flatten_tool_result(&result);
        assert!(text.starts_with("工具执行失败"));
        assert!(text.contains("文件不存在"));
    }

    #[test]
    fn empty_results_still_say_something() {
        assert!(flatten_tool_result(&json!({ "content": [] })).contains("没有返回内容"));
        assert!(flatten_tool_result(&json!({})).contains("没有返回内容"));
    }

    #[test]
    fn a_protocol_level_error_becomes_an_app_error() {
        let err = tool_call_text(None, Some("服务器内部错误")).unwrap_err();
        assert!(err.message.contains("服务器内部错误"));
        // 没有 result 也没有 error：也要报错，不能当成空结果
        assert!(tool_call_text(None, None).is_err());
        assert!(tool_call_text(Some(&json!({ "content": [] })), None).is_ok());
    }
}
