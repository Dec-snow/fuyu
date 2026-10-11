//! 工具注册表：工具描述、执行分发、以及「转成 OpenAI tools 数组」。
//!
//! 与参考项目一致，这里**没有 `Tool` trait**。一个工具 = 一份元数据（
//! [`ToolDefinition`]）+ 一个自由函数，用 [`execute`] 里的 `match` 分发。
//! trait 抽象在这里只会带来 `Box<dyn>` 与生命周期地狱，而多态替换的收益为零。
//!
//! 安全纪律：风险字段（`sensitive` / `exfiltrates`）
//! **只**存在于 [`ToolDefinition`]，[`to_openai_tools`] 导出给模型的只有
//! `name` / `description` / `parameters` —— 模型拿不到任何策略信息。
//!
//! 「是否碰了本地敏感数据」**不在**定义里声明，而是由执行结果
//! [`ToolResult::touched_local_data`] 在运行时报告：只有真正成功读到了本地
//! 内容，才值得触发数据流防泄漏的二次确认。

use serde_json::{json, Value};

/// 一个工具的执行结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// 回灌给模型的内容（成功是结果、失败是错误文案 —— 两种都必须回灌，
    /// 否则下一轮请求会被服务端 400 拒绝）。
    pub content: String,
    /// 这次执行是否读/写了本地敏感数据。
    ///
    /// 这是「数据流防泄漏」的核心信号：置位之后，本轮上下文再发给模型服务商
    /// 前必须让用户再确认一次。
    pub touched_local_data: bool,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            touched_local_data: false,
            is_error: false,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            touched_local_data: false,
            is_error: true,
        }
    }

    /// 标记「这次执行碰了本地敏感数据」。
    pub fn local(mut self) -> Self {
        self.touched_local_data = true;
        self
    }
}

/// 工具元数据（纯数据）。
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    /// 执行前是否需要用户审批。
    pub sensitive: bool,
    /// 是否会把数据外发（联网工具）。
    pub exfiltrates: bool,
}

/// 内置工具清单。加新工具 = 这里加一项 + [`execute`] 加一个分支。
///
/// `web_search_enabled` 为假时**不下发** `web_search`：模型看不到它，
/// 也就不会去调一个没配置的服务。工具清单跟着配置走，而不是让工具
/// 自己报错 —— 后者会白烧一轮 token。
pub fn tool_definitions(web_search_enabled: bool) -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition {
            name: "get_current_time",
            description:
                "获取当前本地日期与时间（含星期几）。当你需要知道「现在几点 / 今天几号」时使用。",
            parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            sensitive: false,
            exfiltrates: false,
        },
        ToolDefinition {
            name: "calculator",
            description: "计算一个只含数字、四则运算（+ - * /）与括号的算术表达式。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "description": "算术表达式，例如 (3 + 4) * 2"
                    }
                },
                "required": ["expression"],
                "additionalProperties": false
            }),
            sensitive: false,
            exfiltrates: false,
        },
        ToolDefinition {
            name: "read_clipboard_text",
            description: "读取系统剪贴板里的文本内容。",
            parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            sensitive: true,
            exfiltrates: false,
        },
        ToolDefinition {
            name: "write_clipboard_text",
            description: "把文本写入系统剪贴板。",
            parameters: json!({
                "type": "object",
                "properties": { "text": { "type": "string", "description": "要写入剪贴板的文本" } },
                "required": ["text"],
                "additionalProperties": false
            }),
            sensitive: true,
            exfiltrates: false,
        },
        ToolDefinition {
            name: "fetch_webpage",
            description:
                "抓取一个网页并转成纯文本。只支持 http / https，且只能访问公网地址（内网与本机地址会被拒绝）。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "要抓取的完整网址，例如 https://example.com/article"
                    }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            // 联网抓取是**数据外发**通道：URL 由模型生成，被提示词注入的模型
            // 完全可以把对话内容塞进查询串带到自己的服务器。所以：
            // - sensitive → 每次都要用户点头；
            // - exfiltrates → can_remember 为 false，不能「永久允许」。
            // 审批卡上会显示完整 URL，用户看到可疑内容就能拒绝。
            sensitive: true,
            exfiltrates: true,
        },
    ];

    if web_search_enabled {
        tools.push(ToolDefinition {
            name: "web_search",
            description:
                "用搜索引擎检索网页，返回标题、网址与摘要。需要先在设置里配置 SearXNG 端点。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "搜索关键词"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            // 搜索同样会把关键词发到第三方；但关键词通常是用户的问题本身，
            // 而不是本地敏感数据，所以允许「本次会话记住」，不必每次点。
            sensitive: true,
            exfiltrates: false,
        });
    }

    tools
}

/// 把工具转成 OpenAI `tools` 数组。
///
/// 只导出 name / description / parameters —— 策略字段绝不发给模型。
pub fn to_openai_tools(web_search_enabled: bool) -> Value {
    Value::Array(
        tool_definitions(web_search_enabled)
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect(),
    )
}

pub fn find(name: &str) -> Option<ToolDefinition> {
    // 搜全部（含按配置隐藏的 web_search）：这里只回答「这个名字是不是内置
    // 工具」，能不能真正执行由 `execute` 决定。
    tool_definitions(true).into_iter().find(|t| t.name == name)
}

/// 工具执行所需的运行时依赖。
pub struct ToolContext {
    /// 用户配置的 SearXNG 端点（未配置为 `None`）。
    pub search_endpoint: Option<String>,
    /// 用户显式配置的 HTTP / SOCKS5 代理（未配置为 `None`）。
    ///
    /// 必须显式传进来：联网工具刻意**不复用**共享的 `reqwest::Client`
    /// （要钉死 IP 就得用 `ClientBuilder::resolve`，而它只能在构造时设置），
    /// 所以每一跳都自己新建客户端。不传的话，用户配了代理也会发现
    /// 「聊天能通、但抓网页和搜索全失败」—— 而且工具会去直连，
    /// 在必须走代理的网络里等于完全不可用。
    pub proxy_url: Option<String>,
}

/// 执行一个工具。`args` 是模型给的参数（已经过 JSON 解析）。
///
/// 声明成 `async` 是因为联网工具要发网络请求；纯本地工具在里面只是不
/// `.await` 而已，代价可以忽略。
pub async fn execute(name: &str, args: &Value, ctx: &ToolContext) -> ToolResult {
    let proxy = ctx.proxy_url.as_deref();
    match name {
        "get_current_time" => get_current_time(),
        "calculator" => calculator(args),
        "read_clipboard_text" => read_clipboard_text(),
        "write_clipboard_text" => write_clipboard_text(args),
        "fetch_webpage" => super::web::run_fetch_webpage(args, proxy).await,
        "web_search" => {
            super::web::run_web_search(ctx.search_endpoint.as_deref(), args, proxy).await
        }
        other => ToolResult::error(format!("未知工具：{other}")),
    }
}

// ------------------------------------------------------------------ 内置工具

fn get_current_time() -> ToolResult {
    let now = chrono::Local::now();
    ToolResult::ok(format!(
        "{}（{}）",
        now.format("%Y-%m-%d %H:%M:%S"),
        now.format("%A")
    ))
}

fn calculator(args: &Value) -> ToolResult {
    let Some(expression) = args.get("expression").and_then(Value::as_str) else {
        return ToolResult::error("缺少 expression 参数");
    };
    match evaluate(expression) {
        Ok(value) => ToolResult::ok(value.to_string()),
        Err(e) => ToolResult::error(format!("计算失败：{e}")),
    }
}

fn read_clipboard_text() -> ToolResult {
    match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
        Ok(text) => ToolResult::ok(if text.is_empty() {
            "（剪贴板为空）".to_string()
        } else {
            text
        })
        .local(),
        Err(e) => ToolResult::error(format!("读取剪贴板失败：{e}")),
    }
}

fn write_clipboard_text(args: &Value) -> ToolResult {
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return ToolResult::error("缺少 text 参数");
    };
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        Ok(()) => ToolResult::ok(format!("已写入剪贴板：{}", truncate(text, 80))).local(),
        Err(e) => ToolResult::error(format!("写入剪贴板失败：{e}")),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

// ------------------------------------------------------------------ 计算器
//
// 手写一个只支持「数字 + 四则运算 + 括号 + 一元正负号」的递归下降解析器。
// 绝不 `eval`、绝不引入表达式库：模型给的是不可信输入，任何能执行代码 / 调用
// 函数的求值器都是注入面。

fn evaluate(expr: &str) -> Result<f64, String> {
    let mut parser = Parser {
        input: expr,
        pos: 0,
    };
    let value = parser.parse_expr()?;
    parser.skip_ws();
    if parser.pos < expr.len() {
        return Err(format!("表达式末尾有多余字符：{}", &expr[parser.pos..]));
    }
    Ok(value)
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn parse_expr(&mut self) -> Result<f64, String> {
        let mut left = self.parse_term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.pos += 1;
                    left += self.parse_term()?;
                }
                Some('-') => {
                    self.pos += 1;
                    left -= self.parse_term()?;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> Result<f64, String> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    left *= self.parse_unary()?;
                }
                Some('/') => {
                    self.pos += 1;
                    let divisor = self.parse_unary()?;
                    if divisor == 0.0 {
                        return Err("除以零".to_string());
                    }
                    left /= divisor;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<f64, String> {
        self.skip_ws();
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Ok(-self.parse_unary()?)
            }
            Some('+') => {
                self.pos += 1;
                self.parse_unary()
            }
            Some('(') => {
                self.pos += 1;
                let value = self.parse_expr()?;
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.pos += 1;
                    Ok(value)
                } else {
                    Err("缺少右括号".to_string())
                }
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.parse_number(),
            Some(c) => Err(format!("无法识别的字符：{c}")),
            None => Err("表达式为空".to_string()),
        }
    }

    fn parse_number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            self.pos += 1;
        }
        let text = &self.input[start..self.pos];
        text.parse::<f64>()
            .map_err(|_| format!("无法解析数字：{text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            search_endpoint: None,
            proxy_url: None,
        }
    }

    fn names(web_search_enabled: bool) -> Vec<&'static str> {
        tool_definitions(web_search_enabled)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn web_search_is_hidden_until_it_is_configured() {
        // 没配搜索服务时不下发这个工具：模型看不到，就不会白烧一轮去调它。
        assert!(!names(false).contains(&"web_search"));
        assert!(names(true).contains(&"web_search"));
        // 其它工具不受影响。
        for always in [
            "get_current_time",
            "calculator",
            "read_clipboard_text",
            "write_clipboard_text",
            "fetch_webpage",
        ] {
            assert!(names(false).contains(&always), "{always} 应当始终可用");
        }
    }

    #[tokio::test]
    async fn an_unconfigured_web_search_explains_how_to_fix_it() {
        let result = execute("web_search", &json!({ "query": "rust" }), &ctx()).await;
        assert!(result.is_error);
        assert!(result.content.contains("设置"), "{}", result.content);
    }

    #[test]
    fn calculator_handles_basic_arithmetic() {
        assert_eq!(evaluate("1 + 2 * 3").unwrap(), 7.0);
        assert_eq!(evaluate("(1 + 2) * 3").unwrap(), 9.0);
        assert_eq!(evaluate("10 / 4").unwrap(), 2.5);
        assert_eq!(evaluate("-3 + 5").unwrap(), 2.0);
        assert_eq!(evaluate("2 * (3 + (4 - 1))").unwrap(), 12.0);
    }

    #[test]
    fn calculator_rejects_unsafe_input() {
        assert!(evaluate("2 + 2").is_ok());
        // 任何超出白名单的字符都拒绝，而不是执行。
        assert!(evaluate("__import__('os')").is_err());
        assert!(evaluate("1; rm -rf /").is_err());
        assert!(evaluate("2 ^ 3").is_err()); // 不支持幂运算（也不是安全风险，只是未实现）
        assert!(evaluate("a + b").is_err());
        assert!(evaluate("").is_err());
    }

    #[test]
    fn calculator_rejects_division_by_zero() {
        assert!(evaluate("1 / 0").is_err());
    }

    #[test]
    fn openai_tools_never_leak_policy_fields() {
        let tools = to_openai_tools(true);
        let serialized = serde_json::to_string(&tools).unwrap();
        assert!(serialized.contains("\"name\""));
        assert!(!serialized.contains("sensitive"), "策略字段绝不能发给模型");
        assert!(!serialized.contains("touches_local_data"));
        assert!(!serialized.contains("exfiltrates"));
    }

    #[tokio::test]
    async fn all_tools_are_discoverable_and_executable() {
        for tool in tool_definitions(true) {
            assert!(find(tool.name).is_some(), "{} 应当能被找到", tool.name);
        }
        // 未知工具给出错误而非 panic。
        assert!(execute("nope", &json!({}), &ctx()).await.is_error);
        // 时间工具不需要参数、不碰本地数据。
        let time = execute("get_current_time", &json!({}), &ctx()).await;
        assert!(!time.is_error);
        assert!(!time.touched_local_data);
    }

    #[tokio::test]
    async fn calculator_tool_flows_through_execute() {
        let result = execute("calculator", &json!({ "expression": "6 * 7" }), &ctx()).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "42");

        let bad = execute("calculator", &json!({}), &ctx()).await;
        assert!(bad.is_error);
        assert!(bad.content.contains("expression"));
    }

    #[tokio::test]
    async fn fetch_webpage_rejects_internal_targets_before_any_request() {
        // 云元数据地址必须在**发请求之前**就被拒绝（这里没有网络也能过）。
        let result = execute(
            "fetch_webpage",
            &json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
            &ctx(),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("拒绝访问"), "{}", result.content);

        // file: 协议同样在入口就被挡下。
        let file = execute(
            "fetch_webpage",
            &json!({ "url": "file:///C:/Windows/win.ini" }),
            &ctx(),
        )
        .await;
        assert!(file.is_error);
        assert!(file.content.contains("只允许 http"), "{}", file.content);

        // 缺参数。
        let missing = execute("fetch_webpage", &json!({}), &ctx()).await;
        assert!(missing.is_error);
        assert!(missing.content.contains("url"));
    }
}
