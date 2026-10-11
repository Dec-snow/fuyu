//! 上下文预算：把「这次到底发多少历史给模型」变成一个有上限、可单测的函数。
//!
//! 为什么必须有它：前端每次提问都会把**整个会话历史**发过来（见
//! `ChatSendRequest::messages`），而校验只挡「条数 ≤ 200、单条 ≤ 20 万字」。
//! 一个聊了很久的会话很容易超过模型的上下文窗口，结果是服务端回一个含糊的
//! 400，用户完全不知道该怎么办。
//!
//! 做法是「从最旧的开始丢，直到装得下」，并把丢了多少条**告诉前端**。
//! 静默截断比报错更糟：用户会以为模型「忘了」前面说过的话。

use crate::models::chat::ChatMessage;

/// 默认上下文预算（token）。
///
/// 3.2 万是个保守值：主流模型的窗口在 32k~128k 之间，留出余量给输出。
pub const DEFAULT_CONTEXT_BUDGET_TOKENS: usize = 32_000;

/// 每条消息的固定开销（role 字段、分隔符等）。
const MESSAGE_OVERHEAD_TOKENS: usize = 4;

/// 无论如何都保留的最近消息条数（至少包含最后那一问一答）。
const MIN_KEEP_MESSAGES: usize = 2;

/// 粗估一段文本的 token 数。
///
/// 刻意**不**做真正的 BPE 分词：那要打包一个几 MB 的词表，而这里只需要一个
/// 保守的上界。经验规则：
/// - CJK 字符约 1 token / 字（国产模型的分词器对中文接近 1:1）
/// - 其他字符约 1 token / 4 字符（英文单词平均 4~5 字符）
///
/// 宁可高估：高估只是多丢一点历史，低估会直接撞上服务端的 400。
pub fn estimate_tokens(text: &str) -> usize {
    let mut wide = 0usize;
    let mut narrow = 0usize;
    for ch in text.chars() {
        if is_wide(ch) {
            wide += 1;
        } else {
            narrow += 1;
        }
    }
    wide + narrow.div_ceil(4)
}

/// 全角 / CJK 字符：这些字符的信息密度高，一个字符就抵一个 token。
fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F       // 韩文字母
        | 0x2E80..=0x303E     // 部首、假名标点
        | 0x3041..=0x33FF     // 假名、注音、CJK 符号
        | 0x3400..=0x4DBF     // CJK 扩展 A
        | 0x4E00..=0x9FFF     // CJK 基本区
        | 0xA000..=0xA4CF     // 彝文
        | 0xAC00..=0xD7A3     // 韩文音节
        | 0xF900..=0xFAFF     // CJK 兼容表意
        | 0xFE30..=0xFE4F     // CJK 兼容形式
        | 0xFF00..=0xFF60     // 全角 ASCII
        | 0xFFE0..=0xFFE6     // 全角符号
        | 0x1F300..=0x1FAFF   // emoji
        | 0x20000..=0x2FA1F   // CJK 扩展 B~
    )
}

/// 一张图片的 token 下限。
///
/// 任何视觉模型处理一张图都至少要几十上百 token（OpenAI 把每张图切成
/// 512px 的 tile，每个 tile 有固定开销），所以小图不能按字节数算成个位数。
const MIN_IMAGE_TOKENS: usize = 256;

/// 估算一张图片（data URL）的 token 数。
///
/// 视觉模型的图片开销是**按分辨率**算的（OpenAI 的口径约 `宽 × 高 / 750`），
/// 而我们手里只有 base64 字符串、没有解码后的尺寸。所以按编码后的长度反推一个
/// 保守上界：压缩图片大致 1 token / 千字节 —— 2 MB 的图约 2000 token，与实际
/// 开销同量级且偏高。
///
/// 取向和 [`estimate_tokens`] 一致：**宁可高估**。高估只是少发几条历史；
/// 低估会让请求带着一堆「免费」的图片直接撞上服务端的上下文上限，
/// 用户只会看到一个含糊的 400。
fn estimate_image_tokens(data_url: &str) -> usize {
    // base64 每 4 个字符对应 3 字节；这里直接用整个 data URL 的长度，
    // 多算了 `data:image/png;base64,` 那二十来个字符，方向上是高估。
    let bytes = data_url.len().saturating_mul(3) / 4;
    (bytes / 1000).max(MIN_IMAGE_TOKENS)
}

pub fn estimate_message_tokens(message: &ChatMessage) -> usize {
    // 图片也要算进预算。漏掉它们的话，一个带图的会话会被当成「几乎不占地方」，
    // 于是裁剪逻辑什么都不丢，请求带着全部图片撞上服务端上限。
    let image_tokens: usize = message
        .images
        .as_ref()
        .map(|images| images.iter().map(|url| estimate_image_tokens(url)).sum())
        .unwrap_or(0);
    estimate_tokens(&message.content) + image_tokens + MESSAGE_OVERHEAD_TOKENS
}

#[derive(Debug, Clone, PartialEq)]
pub struct FittedContext {
    pub messages: Vec<ChatMessage>,
    /// 被**硬丢**掉的消息条数（预算实在装不下，只能丢）。
    pub dropped: usize,
    /// 被**摘要替代**的消息条数。
    ///
    /// 与 `dropped` 是两码事：这部分信息并没有丢，它还在摘要里，只是原文
    /// 不再逐条发送。
    pub summarized: usize,
    /// 估算出的总 token（含 system prompt 与摘要）。
    pub estimated_tokens: usize,
}

impl FittedContext {
    pub fn was_trimmed(&self) -> bool {
        self.dropped > 0
    }
}

/// 摘要消息的前缀：让模型明白这是对更早对话的压缩，而不是用户的新指令。
pub const SUMMARY_PREFIX: &str = "以下是对本次对话更早内容的摘要（原文因长度限制已省略）：\n";

/// 把历史裁到预算之内。
///
/// 规则，按优先级：
/// 1. **最后一条消息永远保留** —— 那是用户这次真正想问的东西，
///    丢掉它比超预算还糟；
/// 2. 至少保留最近 [`MIN_KEEP_MESSAGES`] 条；
/// 3. 其余从**最旧**的开始丢，直到装得下；
/// 4. 裁剪后如果首条是 `assistant`，继续往后挪到第一条 `user` ——
///    以「回答」开头的对话在部分服务端会被直接 400。
pub fn fit_context(
    messages: &[ChatMessage],
    system_prompt: Option<&str>,
    budget_tokens: usize,
) -> FittedContext {
    let base = system_prompt.map(estimate_tokens).unwrap_or(0);
    let per_message: Vec<usize> = messages.iter().map(estimate_message_tokens).collect();
    let total: usize = base + per_message.iter().sum::<usize>();

    if messages.is_empty() || total <= budget_tokens {
        return FittedContext {
            messages: messages.to_vec(),
            dropped: 0,
            summarized: 0,
            estimated_tokens: total,
        };
    }

    // ---- 先满足「至少保留最近 N 条」。
    let min_keep = MIN_KEEP_MESSAGES.clamp(1, messages.len());
    let mut start = messages.len() - min_keep;
    let mut used = base + per_message[start..].iter().sum::<usize>();

    // ---- 再尽量往前多留几条。
    while start > 0 {
        let cost = per_message[start - 1];
        if used + cost > budget_tokens {
            break;
        }
        used += cost;
        start -= 1;
    }

    // ---- 别让上下文以 assistant 开头。
    let last_index = messages.len() - 1;
    let mut adjusted = start;
    while adjusted < last_index && messages[adjusted].role == "assistant" {
        adjusted += 1;
    }
    if adjusted != start {
        used -= per_message[start..adjusted].iter().sum::<usize>();
        start = adjusted;
    }

    FittedContext {
        messages: messages[start..].to_vec(),
        dropped: start,
        summarized: 0,
        estimated_tokens: used,
    }
}

/// 带滚动摘要的上下文装配。
///
/// 比 [`fit_context`] 多两步：
/// 1. 先把摘要覆盖到的原文切掉（信息已经在摘要里，再发一遍纯属浪费预算）；
/// 2. 把摘要当成一条 `system` 消息插在最前面，并从预算里先扣掉它的开销。
///
/// **按 id 定位，不按条数。** `covered_message_id` 指向摘要覆盖到的最后一条
/// 消息；在 `messages` 里找到它，才切掉它之前的部分。
///
/// 为什么不按条数切：数据库里的「已覆盖条数」是**行数**，而这里拿到的是
/// **前端发来的数组**，两者并不等长 —— 前端会过滤掉「正文为空且没有图片」
/// 的消息（历史里的失败轮就是这种），用户「编辑重发」还会把某条之后的
/// 消息整段删掉。位置一旦错位，`messages[covered..]` 就会把**用户刚问的
/// 那句话**当成「已被摘要覆盖」而丢掉：请求发出去只剩摘要，没有提问，
/// 模型对着摘要编一个回答，而且这个问题会一直重复。
///
/// 所以定位失败时**整段放弃摘要**：宁可多发一段历史（甚至因此触发常规裁剪），
/// 也绝不能丢用户的提问。`dropped` 只统计硬丢的条数，被摘要替代的记在 `summarized`。
pub fn fit_context_with_summary(
    messages: &[ChatMessage],
    summary: &str,
    covered_message_id: Option<&str>,
    system_prompt: Option<&str>,
    budget_tokens: usize,
) -> FittedContext {
    let summary = summary.trim();
    if summary.is_empty() {
        return fit_context(messages, system_prompt, budget_tokens);
    }

    // 找不到边界（没记 id / 那条被删了 / 被前端过滤了）→ 放弃摘要。
    let Some(index) =
        covered_message_id.and_then(|id| messages.iter().position(|m| m.id.as_deref() == Some(id)))
    else {
        return fit_context(messages, system_prompt, budget_tokens);
    };

    let tail = &messages[index + 1..];
    if tail.is_empty() {
        // 摘要覆盖到了最后一条。正常不该发生（摘要总会保留最近若干条原文），
        // 真发生了也必须放弃摘要 —— 否则这一轮会把用户的提问整条丢掉。
        return fit_context(messages, system_prompt, budget_tokens);
    }

    let summary_message = ChatMessage {
        role: "system".to_string(),
        content: format!("{SUMMARY_PREFIX}{summary}"),
        ..Default::default()
    };
    let summary_cost = estimate_message_tokens(&summary_message);

    // 摘要自己也吃预算：先扣掉它，再让原文去抢剩下的额度。
    let mut fitted = fit_context(
        tail,
        system_prompt,
        budget_tokens.saturating_sub(summary_cost),
    );
    fitted.summarized = index + 1;
    fitted.estimated_tokens += summary_cost;

    let mut out = Vec::with_capacity(fitted.messages.len() + 1);
    out.push(summary_message);
    out.extend(fitted.messages);
    fitted.messages = out;
    fitted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn chinese_is_about_one_token_per_character() {
        assert_eq!(estimate_tokens("你好世界"), 4);
        // 汉字 + 标点都算全角。
        assert_eq!(estimate_tokens("你好，世界！"), 6);
    }

    #[test]
    fn ascii_is_about_one_token_per_four_chars() {
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("abc"), 1); // 向上取整，宁可高估
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn mixed_text_adds_up() {
        // 4 个汉字 + 8 个 ASCII 字符 = 4 + 2
        assert_eq!(estimate_tokens("你好世界abcdefgh"), 6);
    }

    #[test]
    fn emoji_counts_as_wide() {
        assert_eq!(estimate_tokens("🌍🌏"), 2);
    }

    // ------------------------------------------------------------ 图片算预算

    fn with_image(content: &str, payload_chars: usize) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: content.to_string(),
            images: Some(vec![format!(
                "data:image/png;base64,{}",
                "A".repeat(payload_chars)
            )]),
            ..Default::default()
        }
    }

    #[test]
    fn images_count_against_the_budget() {
        let text_only = msg("user", "看看这张图");
        let with_small = with_image("看看这张图", 1_000);

        // 图片不能是「免费」的：带图的消息必须明显更贵。
        assert!(
            estimate_message_tokens(&with_small) > estimate_message_tokens(&text_only),
            "图片必须计入 token 预算"
        );

        // 一张 2 MB 的图大约两千 token。
        let with_big = with_image("看看这张图", 2_000_000);
        let big = estimate_message_tokens(&with_big);
        assert!(
            big > estimate_message_tokens(&with_small) + 1000,
            "2 MB 的图应当比 1 KB 的贵得多，实际 {big}"
        );

        // 大图比小图贵（单调性）。
        assert!(big > estimate_message_tokens(&with_small));
    }

    #[test]
    fn a_tiny_image_still_has_a_floor() {
        // 5 字节的图不能按字节数算成 0 token —— 任何视觉模型都有固定开销。
        let tiny = with_image("", 4);
        let tokens = estimate_message_tokens(&tiny);
        assert!(tokens >= 256, "小图要有下限，实际 {tokens}");
    }

    #[test]
    fn a_huge_image_forces_trimming_instead_of_being_sent_for_free() {
        // 三张 2 MB 的图 ≈ 7000 token。预算给 3000，就必然要裁掉一部分。
        let messages = vec![
            with_image("图一", 2_000_000),
            with_image("图二", 2_000_000),
            with_image("图三", 2_000_000),
        ];

        let fitted = fit_context(&messages, None, 3_000);

        assert!(
            fitted.dropped > 0,
            "图片必须参与裁剪决策，否则请求会带着全部图片撞上服务端上限"
        );
        // 最后一条永远保留。
        assert_eq!(fitted.messages.last().unwrap().content, "图三");
    }

    // ---------------------------------------------------------- 滚动摘要装配

    /// 造一条带 id 的消息（摘要装配要按 id 定位边界）。
    fn msg_id(id: &str, role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: Some(id.to_string()),
            role: role.to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn summary_is_prepended_and_covered_messages_are_not_resent() {
        let messages = vec![
            msg_id("m1", "user", "第一问"),
            msg_id("m2", "assistant", "第一答"),
            msg_id("m3", "user", "第二问"),
            msg_id("m4", "assistant", "第二答"),
        ];

        // 摘要覆盖到 m2 为止。
        let fitted =
            fit_context_with_summary(&messages, "用户问了两个问题。", Some("m2"), None, 32_000);

        // 摘要作为一条 system 消息打头。
        assert_eq!(fitted.messages.len(), 3);
        assert_eq!(fitted.messages[0].role, "system");
        assert!(fitted.messages[0].content.contains("用户问了两个问题。"));

        // 被摘要覆盖的前两条不再逐条发送。
        assert_eq!(fitted.messages[1].content, "第二问");
        assert_eq!(fitted.messages[2].content, "第二答");

        // 关键区分：这不是「丢了 2 条」，而是「摘要替代了 2 条」。
        assert_eq!(fitted.summarized, 2);
        assert_eq!(fitted.dropped, 0);
        assert!(!fitted.was_trimmed());
    }

    #[test]
    fn an_empty_summary_behaves_exactly_like_plain_fitting() {
        let messages = vec![msg("user", "问"), msg("assistant", "答")];
        let with = fit_context_with_summary(&messages, "", None, Some("系统"), 32_000);
        let plain = fit_context(&messages, Some("系统"), 32_000);

        assert_eq!(with.messages, plain.messages);
        assert_eq!(with.summarized, 0);
        // 空白摘要按「没有摘要」处理，不该插一条空的 system 消息。
        let blank = fit_context_with_summary(&messages, "   \n ", None, Some("系统"), 32_000);
        assert_eq!(blank.messages, plain.messages);
        assert_eq!(blank.summarized, 0);
    }

    #[test]
    fn the_summary_itself_consumes_budget() {
        // 造一段刚好能装下的历史。
        let messages: Vec<_> = (0..6)
            .map(|i| {
                msg_id(
                    &format!("m{i}"),
                    if i % 2 == 0 { "user" } else { "assistant" },
                    "内容",
                )
            })
            .collect();
        let plain = fit_context(&messages, None, 32_000);
        assert_eq!(plain.dropped, 0, "前提：不带摘要时全部装得下");

        // 把预算压到「刚好只够不带摘要的历史 + 一点点」，摘要就会挤掉最早的一条。
        let tail_cost: usize = 6 * 4 + estimate_tokens("内容") * 6;
        let summary = "很长的摘要".repeat(6);
        let with_summary =
            fit_context_with_summary(&messages, &summary, Some("m1"), None, tail_cost + 4);

        assert!(
            with_summary.dropped > 0,
            "摘要自身的开销必须计入预算，实际 dropped = {}",
            with_summary.dropped
        );
        // 摘要仍然在，且总估算不超过预算加一点余量。
        assert_eq!(with_summary.messages[0].role, "system");
    }

    /// 回归测试：这一条曾经是**真 bug**。
    ///
    /// 前端会过滤掉「正文为空且没有图片」的消息，用户「编辑重发」还会删掉
    /// 一整段消息 —— 于是数据库里记的「已覆盖条数」会大于、甚至远远大于
    /// 这次真正收到的消息条数。旧实现用 `messages[covered..]` 切，
    /// 结果是把**用户刚问的那句话**当成「已被摘要覆盖」整条丢掉：
    /// 请求发出去只剩摘要、没有提问，模型对着摘要编一个回答。
    #[test]
    fn a_stale_boundary_never_swallows_the_current_question() {
        // 摘要声称覆盖了 40 条，但真实数组只剩 6 条（用户编辑后删掉了后面全部）。
        let messages = vec![
            msg_id("m1", "user", "第一问"),
            msg_id("m2", "assistant", "第一答"),
            msg_id("m3", "user", "第二问"),
            msg_id("m4", "assistant", "第二答"),
            msg_id("m5", "user", "第三问"),
            msg_id("m6", "user", "刚改过的那句话"),
        ];

        // 边界 id 指向一条**已经不存在**的消息（它被删掉了）。
        let fitted = fit_context_with_summary(
            &messages,
            "一份过期的摘要",
            Some("msg_已被删除"),
            None,
            32_000,
        );

        // 定位不到边界 → 整段放弃摘要，历史照发。
        assert_eq!(fitted.summarized, 0, "定位失败时不该声称用了摘要");
        assert!(
            !fitted.messages.iter().any(|m| m.role == "system"),
            "不该插入摘要消息"
        );
        assert_eq!(fitted.messages.len(), 6, "全部历史都要发出去");
        assert_eq!(
            fitted.messages.last().unwrap().content,
            "刚改过的那句话",
            "用户的提问绝对不能丢"
        );
    }

    #[test]
    fn a_boundary_on_the_last_message_also_abandons_the_summary() {
        // 极端情况：摘要覆盖到了最后一条。用摘要就等于「只发摘要、不发提问」。
        let messages = vec![
            msg_id("m1", "user", "旧问题"),
            msg_id("m2", "assistant", "旧回答"),
        ];

        let fitted = fit_context_with_summary(&messages, "旧事摘要", Some("m2"), None, 32_000);

        assert_eq!(fitted.summarized, 0);
        assert_eq!(fitted.messages.len(), 2, "历史要原样发出");
        assert_eq!(fitted.messages.last().unwrap().content, "旧回答");
    }

    #[test]
    fn a_boundary_that_is_absent_but_the_summary_is_still_there_is_ignored() {
        // 没有 id（老数据 / 摘要刚写一半）时同样放弃摘要，而不是退化成按条数切。
        let messages = vec![msg_id("m1", "user", "只有一条")];
        let fitted = fit_context_with_summary(&messages, "摘要", None, None, 32_000);

        assert_eq!(fitted.summarized, 0);
        assert_eq!(fitted.messages.len(), 1);
        assert_eq!(fitted.messages[0].content, "只有一条");
    }

    #[test]
    fn a_zero_budget_still_keeps_the_summary_and_the_last_turn() {
        let messages = vec![
            msg_id("m1", "user", "旧问题"),
            msg_id("m2", "assistant", "旧回答"),
            msg_id("m3", "user", "新问题"),
        ];
        let fitted = fit_context_with_summary(&messages, "旧事摘要", Some("m2"), None, 0);

        // 摘要 + 最后一条用户消息：预算为 0 也不能把「这次问的是什么」丢掉。
        assert_eq!(fitted.messages[0].role, "system");
        assert_eq!(
            fitted.messages.last().unwrap().content,
            "新问题",
            "最后一条消息必须保留"
        );
    }

    #[test]
    fn a_conversation_that_fits_is_left_alone() {
        let messages = vec![msg("user", "你好"), msg("assistant", "在的")];
        let fitted = fit_context(&messages, Some("你是助手"), 32_000);

        assert_eq!(fitted.messages, messages);
        assert_eq!(fitted.dropped, 0);
        assert!(!fitted.was_trimmed());
    }

    #[test]
    fn oldest_messages_are_dropped_first() {
        let mut messages = Vec::new();
        for i in 0..40 {
            messages.push(msg(
                "user",
                &format!("第 {i} 个问题，{}", "内容".repeat(20)),
            ));
            messages.push(msg("assistant", &"回答".repeat(20)));
        }

        let fitted = fit_context(&messages, None, 200);

        assert!(fitted.was_trimmed());
        assert!(fitted.dropped > 0);
        // 最新的那条一定还在。
        assert_eq!(fitted.messages.last().unwrap(), messages.last().unwrap());
        // 丢掉的一定是最旧的：留下的必须是原数组的一个后缀。
        assert_eq!(
            fitted.messages.as_slice(),
            &messages[fitted.dropped..],
            "只能从最旧的开始丢，不能挖空中间"
        );
        assert!(fitted.estimated_tokens <= 200);
    }

    #[test]
    fn the_last_message_survives_even_if_it_alone_blows_the_budget() {
        let messages = vec![
            msg("user", &"很长的历史".repeat(500)),
            msg("assistant", &"很长的回答".repeat(500)),
            msg("user", &"这一条本身就超预算".repeat(500)),
        ];

        let fitted = fit_context(&messages, None, 10);

        assert_eq!(fitted.messages.len(), 1, "至少保留最后一条");
        assert_eq!(fitted.messages[0], messages[2]);
        assert!(fitted.estimated_tokens > 10, "明知超预算也要发出去");
    }

    #[test]
    fn at_least_the_recent_turns_are_kept() {
        let messages = vec![
            msg("user", "甲"),
            msg("assistant", "乙"),
            msg("user", "丙"),
            msg("assistant", "丁"),
        ];
        // 预算只够放最近两条。
        let fitted = fit_context(&messages, None, 12);

        assert!(fitted.messages.len() >= MIN_KEEP_MESSAGES);
        assert_eq!(fitted.messages.last().unwrap().content, "丁");
    }

    #[test]
    fn the_system_prompt_counts_against_the_budget() {
        // 同一段历史、同一个预算，只把系统提示词加上去 —— 应当少留几条。
        // 这样构造才能真的证明「系统提示词被算进了预算」，而不是碰巧超限。
        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(msg("user", &format!("问题{i} {}", "内容".repeat(10))));
            messages.push(msg("assistant", &format!("回答{i} {}", "回答".repeat(10))));
        }
        let prompt = "很长的系统提示词".repeat(400);

        let without = fit_context(&messages, None, 400);
        let with = fit_context(&messages, Some(&prompt), 400);

        assert!(without.was_trimmed(), "这段历史本身就该被裁");
        assert!(with.was_trimmed());
        assert!(
            with.dropped > without.dropped,
            "系统提示词占了预算，应当多丢一些：带提示词丢了 {} 条，不带丢了 {} 条",
            with.dropped,
            without.dropped
        );
        assert_eq!(
            with.messages.last().unwrap(),
            messages.last().unwrap(),
            "无论怎么裁，最后一条都要在"
        );
    }

    #[test]
    fn trimmed_context_never_starts_with_an_assistant_message() {
        // 构造一个「切点正好落在一问一答中间」的场景。
        let mut messages = vec![msg("system", "系统")];
        for i in 0..20 {
            messages.push(msg("user", &format!("问题{i} {}", "x".repeat(40))));
            messages.push(msg("assistant", &format!("回答{i} {}", "y".repeat(40))));
        }

        for budget in [60usize, 80, 100, 120, 160, 200] {
            let fitted = fit_context(&messages, None, budget);
            assert_ne!(
                fitted.messages.first().map(|m| m.role.as_str()),
                Some("assistant"),
                "预算 {budget} 时上下文以 assistant 开头的，部分服务端会 400"
            );
        }
    }

    #[test]
    fn empty_history_is_handled() {
        let fitted = fit_context(&[], Some("系统提示"), 100);
        assert!(fitted.messages.is_empty());
        assert_eq!(fitted.dropped, 0);
        // 系统提示词的 token 仍然要报出来。
        assert!(fitted.estimated_tokens > 0);
    }

    #[test]
    fn a_zero_budget_degrades_to_the_last_message() {
        let messages = vec![msg("user", "一"), msg("assistant", "二"), msg("user", "三")];
        let fitted = fit_context(&messages, None, 0);

        assert_eq!(fitted.messages.len(), 1);
        assert_eq!(fitted.messages[0].content, "三");
    }
}
