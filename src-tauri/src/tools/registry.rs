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

/// 哪些「有条件」的工具该下发给模型。
///
/// 为什么做成结构体而不是继续加 bool 参数：条件工具会越来越多（联网搜索、
/// 知识库检索、将来的 MCP），每加一个就要改一遍所有调用点的签名，而且
/// `tool_definitions(true)` 这种写法，过一阵就没人记得那个 `true` 是什么了。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolAvailability {
    /// 配了 SearXNG 端点才下发 `web_search`。
    pub web_search: bool,
    /// 知识库里有文档才下发 `search_knowledge_base`。
    pub knowledge_base: bool,
    /// 打开了长期记忆才下发 `remember`。
    pub memory: bool,
}

impl ToolAvailability {
    /// 全部可用。给「按名字查工具定义」和测试用。
    pub fn all() -> Self {
        Self {
            web_search: true,
            knowledge_base: true,
            memory: true,
        }
    }
}

/// 内置工具清单。加新工具 = 这里加一项 + [`execute`] 加一个分支。
///
/// 条件工具在条件不满足时**不下发**：模型看不到它，也就不会去调一个没配置
/// 的服务。工具清单跟着配置走，而不是让工具自己报错 —— 后者会白烧一轮 token。
pub fn tool_definitions(availability: ToolAvailability) -> Vec<ToolDefinition> {
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

    if availability.web_search {
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

    if availability.knowledge_base {
        tools.push(ToolDefinition {
            name: "search_knowledge_base",
            description:
                "在用户自己导入的本地知识库里检索（BM25 全文检索），返回最相关的若干片段与来源文档。\
                 当问题涉及用户导入的资料、笔记、手册时使用；它只能看到用户已经导入的内容。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "检索关键词或一句话问题"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "返回几个片段（1-20，默认 5）"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            // 纯本地检索：数据不出机器，也不碰系统资源，所以不敏感、不过滤外发。
            sensitive: false,
            exfiltrates: false,
        });
    }

    if availability.memory {
        tools.push(ToolDefinition {
            name: "remember",
            description: "把关于用户的、**以后还用得上**的事实记下来（长期偏好、正在做的项目、\
                         固定的工作习惯）。只记长期有效的信息；一次性的事情、能从这次对话里\
                         直接看出来的、以及任何敏感信息都不要记。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "一句话说清要记住的事实，例如「用户偏好 Rust 而不是 Python」"
                    }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            // 只是写本地的一张表：不外发、不碰系统资源。
            // 用户能在「设置 → 知识库」里看到并逐条删除，所以不必每次弹审批。
            sensitive: false,
            exfiltrates: false,
        });
    }

    tools
}

/// 工具清单，按技能的工具白名单过滤。
///
/// `whitelist` 为 `None` 表示不限制（沿用全局设置）；为 `Some` 时**只保留**
/// 名单里的工具。
///
/// 一个必须说清的点：白名单**只能收窄，不能放开**。像 `web_search` 这种工具
/// 没配端点时本来就不下发，技能点名它也不会凭空能用 —— 那只会让模型去调一个
/// 必然失败的工具，白烧一轮 token（这条原则写在 [`tool_definitions`] 上面）。
pub fn tools_for(
    availability: ToolAvailability,
    whitelist: Option<&[String]>,
) -> Vec<ToolDefinition> {
    let tools = tool_definitions(availability);
    let Some(whitelist) = whitelist else {
        return tools;
    };
    tools
        .into_iter()
        .filter(|tool| whitelist.iter().any(|name| name == tool.name))
        .collect()
}

/// 把工具转成 OpenAI `tools` 数组。
///
/// 只导出 name / description / parameters —— 策略字段绝不发给模型。
pub fn to_openai_tools(availability: ToolAvailability, whitelist: Option<&[String]>) -> Value {
    Value::Array(
        tools_for(availability, whitelist)
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
    // 搜全部（含按配置隐藏的 web_search / search_knowledge_base）：
    // 这里只回答「这个名字是不是内置工具」，能不能真正执行由 `execute` 决定。
    tool_definitions(ToolAvailability::all())
        .into_iter()
        .find(|t| t.name == name)
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
    /// 知识库句柄（库里没有文档时为 `None`）。
    ///
    /// `Database` 内部是 `Arc<Mutex<Connection>>`，克隆很便宜；
    /// 传进来的目的是让检索工具能直接查库，而不是再包一层服务。
    pub knowledge_base: Option<crate::storage::database::Database>,
    /// 长期记忆的句柄（没打开长期记忆时为 `None`）。
    pub memory: Option<crate::storage::database::Database>,
    /// 本次允许使用的工具名（`None` = 不限制）。
    ///
    /// **执行时也要按它拦住**，不能只在清单里过滤：清单只是「模型能看到什么」，
    /// 模型完全可能凭名字硬调一个没下发的工具。对「纯离线总结」这种技能来说，
    /// 被硬调一次 `web_search` 就意味着数据发出去了 —— 那是这个技能存在的
    /// 全部意义所在，所以必须在服务端拦住（与文档解析的上限同一个道理：
    /// 前端的限制拦不住直接调命令的人）。
    pub allowed: Option<Vec<String>>,
}

/// 执行一个工具。`args` 是模型给的参数（已经过 JSON 解析）。
///
/// 声明成 `async` 是因为联网工具要发网络请求；纯本地工具在里面只是不
/// `.await` 而已，代价可以忽略。
pub async fn execute(name: &str, args: &Value, ctx: &ToolContext) -> ToolResult {
    // 技能白名单在这里也要拦一道（理由见 `ToolContext::allowed`）
    if let Some(allowed) = &ctx.allowed {
        if !allowed.iter().any(|n| n == name) {
            return ToolResult::error(format!(
                "当前技能没有开启「{name}」这个工具，不要改用别的途径绕过它"
            ));
        }
    }

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
        "search_knowledge_base" => search_knowledge_base(args, ctx.knowledge_base.as_ref()),
        "remember" => remember(args, ctx.memory.as_ref()),
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

/// 知识库检索工具。
///
/// 输出刻意带上**来源与位置**（文档标题 + 第几片）：模型引用时能说清
/// 「这来自哪份资料的哪一段」，用户也才能核对。
///
/// 刻意**不把 score 发给模型**：一个 `0.8735` 之类的数字对它没有意义，
/// 只会被原样抄进回答里（「根据资料（相关度 0.87）…」），看着像那么回事，
/// 其实是在展示一个对读者毫无信息量的内部指标。
fn search_knowledge_base(
    args: &Value,
    db: Option<&crate::storage::database::Database>,
) -> ToolResult {
    let Some(db) = db else {
        return ToolResult::error("知识库还没有内容（可先在「设置 → 知识库」里导入文档）");
    };
    let query = match args.get("query").and_then(Value::as_str) {
        Some(q) if !q.trim().is_empty() => q,
        _ => return ToolResult::error("缺少 query 参数"),
    };
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(crate::kb::DEFAULT_SEARCH_LIMIT as u64) as usize;

    match db.kb_search(query, limit) {
        Ok(hits) if hits.is_empty() => {
            ToolResult::ok("知识库里没有找到相关内容。（这不代表用户没问过，换几个关键词再试）")
        }
        Ok(hits) => {
            let mut out = String::new();
            for (index, hit) in hits.iter().enumerate() {
                out.push_str(&format!(
                    "【{}】{}（第 {} 片）\n{}\n\n",
                    index + 1,
                    hit.title,
                    hit.ordinal,
                    hit.text
                ));
            }
            ToolResult::ok(out.trim_end())
        }
        Err(e) => ToolResult::error(format!("知识库检索失败：{e}")),
    }
}

/// 记住一条关于用户的事实。
///
/// 返回里明确告诉模型「去重时是更新了已有的一条」：它会据此避免
/// 换个说法反复记同一件事（虽然服务端也会去重，但让它知道更省 token）。
fn remember(args: &Value, db: Option<&crate::storage::database::Database>) -> ToolResult {
    let Some(db) = db else {
        return ToolResult::error("长期记忆没有打开（可在「设置 → 知识库」里开启）");
    };
    let text = match args.get("text").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => t,
        _ => return ToolResult::error("缺少 text 参数"),
    };

    match db.memory_add(text, "tool") {
        Ok(outcome) if outcome.merged => {
            ToolResult::ok(format!("已更新已有的一条记忆：{}", outcome.memory.text))
        }
        Ok(outcome) => ToolResult::ok(format!("已记住：{}", outcome.memory.text)),
        Err(e) => ToolResult::error(format!("记不下来：{e}")),
    }
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
            knowledge_base: None,
            memory: None,
            allowed: None,
        }
    }

    fn names(availability: ToolAvailability) -> Vec<&'static str> {
        tool_definitions(availability)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn web_search_is_hidden_until_it_is_configured() {
        // 没配搜索服务时不下发这个工具：模型看不到，就不会白烧一轮去调它。
        assert!(!names(ToolAvailability::default()).contains(&"web_search"));
        assert!(names(ToolAvailability::all()).contains(&"web_search"));
        // 其它工具不受影响。
        for always in [
            "get_current_time",
            "calculator",
            "read_clipboard_text",
            "write_clipboard_text",
            "fetch_webpage",
        ] {
            assert!(
                names(ToolAvailability::default()).contains(&always),
                "{always} 应当始终可用"
            );
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
    fn knowledge_base_tool_is_hidden_until_the_library_has_documents() {
        // 与 web_search 同一个原则：库是空的就不下发 —— 否则模型会去调一个
        // 必然空手而归的工具，白烧一轮 token。
        assert!(!names(ToolAvailability::default()).contains(&"search_knowledge_base"));
        assert!(names(ToolAvailability::all()).contains(&"search_knowledge_base"));
    }

    #[tokio::test]
    async fn knowledge_base_tool_without_a_handle_explains_itself() {
        // 工具被直接调用（绕过清单）时也要给人话，而不是 panic
        let result = execute("search_knowledge_base", &json!({ "query": "x" }), &ctx()).await;
        assert!(result.is_error);
        assert!(result.content.contains("知识库"), "{}", result.content);
    }

    #[tokio::test]
    async fn knowledge_base_tool_returns_sources_and_hides_scores() {
        // 用真库跑一遍：输出要带来源（文档标题 + 第几片），
        // 但**不能**带 score —— 那个数字会被模型原样抄进回答里。
        let dir =
            std::env::temp_dir().join(format!("floatai-kbtool-{}", crate::storage::new_id("")));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::storage::database::Database::open(&dir.join("kb.db")).unwrap();
        db.kb_add_document(
            "浮语手册",
            "manual.md",
            "浮语是一个桌面 AI 助手。它支持全局热键 Alt+Space 唤出悬浮窗。",
        )
        .unwrap();
        db.kb_add_document("无关文档", "other.md", "今天天气不错，适合出门散步。")
            .unwrap();

        let kb_ctx = ToolContext {
            search_endpoint: None,
            proxy_url: None,
            knowledge_base: Some(db),
            memory: None,
            allowed: None,
        };
        let result = execute(
            "search_knowledge_base",
            &json!({ "query": "全局热键" }),
            &kb_ctx,
        )
        .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content.contains("浮语手册"),
            "应当带来源：{}",
            result.content
        );
        assert!(
            result.content.contains("第 1 片"),
            "应当标出片号：{}",
            result.content
        );
        assert!(result.content.contains("Alt+Space"));
        assert!(
            !result.content.contains("score"),
            "不该把内部打分发给模型：{}",
            result.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remember_tool_is_hidden_until_long_term_memory_is_on() {
        // 与 web_search / 知识库同一个原则：没打开就不下发
        let off = ToolAvailability {
            web_search: false,
            knowledge_base: false,
            memory: false,
        };
        assert!(!names(off).contains(&"remember"));
        assert!(names(ToolAvailability::all()).contains(&"remember"));
    }

    #[tokio::test]
    async fn remember_tool_without_a_handle_explains_itself() {
        let result = execute("remember", &json!({ "text": "x" }), &ctx()).await;
        assert!(result.is_error);
        assert!(result.content.contains("长期记忆"), "{}", result.content);
    }

    #[tokio::test]
    async fn remember_tool_writes_and_reports_merges() {
        let dir =
            std::env::temp_dir().join(format!("floatai-memtool-{}", crate::storage::new_id("")));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::storage::database::Database::open(&dir.join("mem.db")).unwrap();
        let mem_ctx = ToolContext {
            search_endpoint: None,
            proxy_url: None,
            knowledge_base: None,
            memory: Some(db.clone()),
            allowed: None,
        };

        let first = execute(
            "remember",
            &json!({ "text": "用户偏好 Rust 而不是 Python" }),
            &mem_ctx,
        )
        .await;
        assert!(!first.is_error, "{}", first.content);
        assert!(first.content.contains("已记住"), "{}", first.content);
        assert_eq!(db.memory_count().unwrap(), 1);

        // 换个说法再来一次：应当报「已更新」，而且**不新增**
        let again = execute(
            "remember",
            &json!({ "text": "用户偏好用 Rust 而不是 Python" }),
            &mem_ctx,
        )
        .await;
        assert!(again.content.contains("已更新"), "{}", again.content);
        assert_eq!(db.memory_count().unwrap(), 1, "换个说法不该攒成两条");

        // 缺参数要给人话
        let missing = execute("remember", &json!({}), &mem_ctx).await;
        assert!(missing.is_error);
        assert!(missing.content.contains("text"), "{}", missing.content);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_whitelist_keeps_only_the_named_tools() {
        let allow = vec!["calculator".to_string(), "get_current_time".to_string()];
        let names: Vec<&str> = tools_for(ToolAvailability::all(), Some(&allow))
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["get_current_time", "calculator"]);
        assert!(!names.contains(&"web_search"));
        assert!(!names.contains(&"read_clipboard_text"));
    }

    #[test]
    fn a_whitelist_cannot_unlock_a_tool_that_is_not_available() {
        // 「联网调研」技能点名了 web_search，但用户根本没配搜索端点 ——
        // 此时不能凭空把它放出去：模型会去调一个必然失败的工具，白烧一轮 token。
        let allow = vec!["web_search".to_string()];
        let off = ToolAvailability {
            web_search: false,
            knowledge_base: false,
            memory: false,
        };
        let names: Vec<&str> = tools_for(off, Some(&allow))
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(names.is_empty(), "白名单只能收窄，不能放开：{names:?}");

        // 配了端点就该出现
        let on = ToolAvailability {
            web_search: true,
            ..off
        };
        let names: Vec<&str> = tools_for(on, Some(&allow))
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["web_search"]);
    }

    #[tokio::test]
    async fn execute_refuses_a_tool_outside_the_whitelist() {
        // 这是「纯离线总结」技能成立的前提：清单只是「模型能看到什么」，
        // 模型完全可以凭名字硬调一个没下发的工具。被硬调一次 web_search
        // 就意味着数据发出去了 —— 所以执行时也要拦。
        let scoped = ToolContext {
            search_endpoint: None,
            proxy_url: None,
            knowledge_base: None,
            memory: None,
            allowed: Some(vec!["calculator".to_string()]),
        };

        let blocked = execute("get_current_time", &json!({}), &scoped).await;
        assert!(blocked.is_error, "名单外的工具必须被拒");
        assert!(blocked.content.contains("没有开启"), "{}", blocked.content);

        // 名单内的照常能跑
        let allowed = execute("calculator", &json!({ "expression": "6 * 7" }), &scoped).await;
        assert!(!allowed.is_error, "{}", allowed.content);
        assert!(allowed.content.contains("42"));
    }

    #[test]
    fn openai_tools_never_leak_policy_fields() {
        let tools = to_openai_tools(ToolAvailability::all(), None);
        let serialized = serde_json::to_string(&tools).unwrap();
        assert!(serialized.contains("\"name\""));
        assert!(!serialized.contains("sensitive"), "策略字段绝不能发给模型");
        assert!(!serialized.contains("touches_local_data"));
        assert!(!serialized.contains("exfiltrates"));
    }

    #[tokio::test]
    async fn all_tools_are_discoverable_and_executable() {
        for tool in tool_definitions(ToolAvailability::all()) {
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
