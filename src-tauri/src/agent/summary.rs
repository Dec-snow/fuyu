//! 滚动摘要：把「最早那段装不下的历史」压缩成一段摘要，而不是直接丢掉。
//!
//! 为什么需要它：`context::fit_context` 的做法是「从最旧的开始丢」，聊久了
//! 用户会发现模型把前面说过的事**彻底忘了** —— 丢了就是真丢了。滚动摘要把
//! 那部分历史交给模型自己压成几段话留在提示词里，信息还在。
//!
//! 分工：**这里只做纯逻辑**（该不该摘要、要折叠哪一段、提示词怎么拼）。
//! 真正的网络调用在 `commands::chat` 里，落库在 `storage::database`。

use std::ops::Range;

use crate::models::chat::{ChatMessage, MessageText};

use super::context::estimate_tokens;

/// 会话消息条数超过这个值，才考虑做摘要。
///
/// 定得太低会让短对话也白白多花一次 API 调用；太高则摘要永远追不上增长。
pub const SUMMARY_TRIGGER_MESSAGES: usize = 24;

/// 每次摘要都**保留最近这么多条原文**不压缩。
///
/// 最近的对话细节最重要（用户可能正在追问上一个回答），压成摘要会丢细节。
pub const SUMMARY_KEEP_RECENT: usize = 8;

/// 摘要正文的字符上限。摘要自己也要吃上下文预算，不能让它无限膨胀。
pub const MAX_SUMMARY_CHARS: usize = 2_000;

/// 判断这次要不要做摘要；要做的话返回**本次要折叠进摘要的消息区间**。
///
/// `roles` 是整个会话的角色序列；`covered` 是现有摘要已经覆盖的条数
/// （没有摘要时传 0）。
///
/// 折叠边界会向后调整到一条 `user` 消息：把「提问」折进摘要、却把它的
/// 「回答」留在原文里，会让剩下的上下文以 `assistant` 开头，不少服务端
/// 会因此直接 400。
pub fn plan_summary(roles: &[&str], covered: usize) -> Option<Range<usize>> {
    let total = roles.len();
    if total <= SUMMARY_TRIGGER_MESSAGES {
        return None;
    }

    let mut end = total - SUMMARY_KEEP_RECENT;
    while end < total && roles.get(end) != Some(&"user") {
        end += 1;
    }

    // 没有新内容可折，或者一折就折到尾部（等于没保留最近若干条）。
    if end <= covered || end >= total {
        return None;
    }
    Some(covered..end)
}

/// 折叠给模型看的原文，最多占多少 token。
///
/// 不设上限的话：一个贴过超长文档的会话，第一次折叠就会构造出一个超出服务商
/// 上限的请求 —— 它**永远**失败，而此后每一次成功的问答之后都会原样重试一遍，
/// 白烧一次 API 调用；更糟的是覆盖进度永远不前进，摘要功能等于彻底废掉。
pub const MAX_SUMMARY_INPUT_TOKENS: usize = 12_000;

/// 把折叠区间收缩到「提示词装得下」为止。
///
/// 从区间**起点**往后累加，能装多少折多少，剩下的留到下一轮 ——
/// 这样覆盖进度单调前进，超大历史最终也能折完。
///
/// 边界优先落在 `user` 消息上：切在别处会让剩下的原文以 `assistant` 开头。
/// 一条都装不下时也要折**一条**，否则进度会永远卡住。
pub fn shrink_to_budget(
    roles: &[&str],
    contents: &[String],
    range: Range<usize>,
    budget_tokens: usize,
) -> Option<Range<usize>> {
    if range.is_empty() || range.start >= contents.len() {
        return None;
    }
    let end_limit = range.end.min(contents.len());

    let mut used = 0usize;
    let mut best: Option<usize> = None;

    for (index, content) in contents
        .iter()
        .enumerate()
        .take(end_limit)
        .skip(range.start)
    {
        used = used.saturating_add(estimate_tokens(content));
        let next = index + 1;

        // 边界候选：要么正好是原区间末尾，要么下一条是 user 消息。
        let boundary_ok = next == range.end || roles.get(next) == Some(&"user");
        if boundary_ok && used <= budget_tokens {
            best = Some(next);
        }
    }

    match best {
        Some(end) if end > range.start => Some(range.start..end),
        _ => {
            // 连一条都装不下（单条就超预算）：也必须折一条，让进度往前走。
            // 优先折到第一个 user 边界，实在没有就折第一条。
            for index in range.start..end_limit {
                let next = index + 1;
                if roles.get(next) == Some(&"user") {
                    return Some(range.start..next);
                }
            }
            Some(range.start..(range.start + 1).min(end_limit))
        }
    }
}

/// 从折叠区间里挑一条「摘要边界」消息 id，写回数据库供下一轮定位。
///
/// 必须是**下一轮一定还会出现在历史数组里**的那条消息：前端会丢掉
/// 「正文为空且没有图片」的消息（历史里的失败轮正是如此），拿它当边界会导致
/// 下一轮按 id 定位失败、摘要整个白做。所以从区间末尾往前找第一条
/// 「有正文或有图片」的消息。
///
/// 区间里一条这样的消息都没有时返回 `None` —— 那说明这段历史全是空消息，
/// 没有摘要的价值。
pub fn covered_message_id(texts: &[MessageText], range: &Range<usize>) -> Option<String> {
    texts
        .get(range.clone())?
        .iter()
        .rev()
        .find(|message| !message.content.trim().is_empty() || message.has_images)
        .map(|message| message.id.clone())
}

/// 拼出「请压缩这段历史」的请求消息。
///
/// `previous_summary` 非空时是**增量合并**：把新对话并进已有摘要，而不是
/// 从头重写 —— 否则每次摘要都要重读全部历史，成本和延迟都随会话长度增长。
pub fn build_summary_messages(
    previous_summary: &str,
    messages: &[ChatMessage],
) -> Vec<ChatMessage> {
    let instruction = if previous_summary.trim().is_empty() {
        "请把下面这段对话压缩成一段简洁的中文摘要。必须保留：用户的目标与偏好、\
         已经确认的事实与结论、尚未解决的问题、以及后续对话必须遵守的约束。\
         不要编造对话里没有的信息，不要写「用户说」这类转述，直接陈述要点。"
    } else {
        "下面是一份已有的对话摘要，以及之后新增的对话。请把新增内容合并进去，\
         输出一份更新后的完整摘要。同样保留目标、已确认的事实、未决问题与约束；\
         已有信息不要丢失，被推翻或过时的结论要更新。"
    };

    let mut transcript = String::new();
    for message in messages {
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        let speaker = match message.role.as_str() {
            "user" => "用户",
            "assistant" => "助手",
            other => other,
        };
        transcript.push_str(speaker);
        transcript.push('：');
        transcript.push_str(content);
        transcript.push('\n');
    }

    let mut body = String::new();
    if !previous_summary.trim().is_empty() {
        body.push_str("【已有摘要】\n");
        body.push_str(previous_summary.trim());
        body.push_str("\n\n");
    }
    body.push_str("【新增对话】\n");
    body.push_str(&transcript);
    body.push_str("\n请直接输出摘要正文，不要任何前缀或解释。");

    vec![
        ChatMessage {
            role: "system".to_string(),
            content: instruction.to_string(),
            ..Default::default()
        },
        ChatMessage {
            role: "user".to_string(),
            content: body,
            ..Default::default()
        },
    ]
}

/// 清理模型返回的摘要。
///
/// 有些模型习惯把整段内容包进 ``` 代码块，或者加一句「以下是摘要：」。
/// 这些都会污染下一轮的提示词，所以在这里剥掉，并截到长度上限。
pub fn clean_summary(raw: &str) -> String {
    let mut text = raw.trim().to_string();

    if text.starts_with("```") {
        text = text.trim_start_matches("```").to_string();
        // 去掉紧跟的语言标识（```markdown 之类）。
        if let Some(newline) = text.find('\n') {
            let (first, rest) = text.split_at(newline);
            if !first.contains('：') && first.chars().count() <= 12 {
                text = rest.to_string();
            }
        }
        if let Some(stripped) = text.trim_end().strip_suffix("```") {
            text = stripped.to_string();
        }
        text = text.trim().to_string();
    }

    if text.chars().count() > MAX_SUMMARY_CHARS {
        text = text.chars().take(MAX_SUMMARY_CHARS).collect();
        text.push('…');
    }
    text
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

    /// 造一段「u/a 交替」的角色序列。
    fn alternating(pairs: usize) -> Vec<&'static str> {
        let mut roles = Vec::new();
        for _ in 0..pairs {
            roles.push("user");
            roles.push("assistant");
        }
        roles
    }

    #[test]
    fn short_conversations_are_not_summarized() {
        let roles = alternating(SUMMARY_TRIGGER_MESSAGES / 2);
        assert!(roles.len() <= SUMMARY_TRIGGER_MESSAGES);
        assert!(plan_summary(&roles, 0).is_none());
    }

    #[test]
    fn long_conversations_fold_the_oldest_part_and_keep_the_recent_tail() {
        let roles = alternating(20); // 40 条
        let range = plan_summary(&roles, 0).expect("应当要摘要");

        // 至少保留最近 SUMMARY_KEEP_RECENT 条原文。
        assert!(
            roles.len() - range.end >= SUMMARY_KEEP_RECENT,
            "折叠后剩下的原文太少：{range:?}"
        );
        // 折叠区间从 0 开始（首次摘要）。
        assert_eq!(range.start, 0);
        // 边界必须落在 user 上，否则剩下的上下文会以 assistant 开头。
        assert_eq!(roles[range.end], "user");
    }

    #[test]
    fn boundary_lands_on_a_user_message_even_with_odd_tails() {
        // 尾部是 user 结尾（奇数列），边界需要往后挪一条。
        let mut roles = alternating(20);
        roles.push("user"); // 41 条，最后一条是 user
        let range = plan_summary(&roles, 0).expect("应当要摘要");
        assert_eq!(roles[range.end], "user");
        assert!(range.end < roles.len());
    }

    #[test]
    fn nothing_new_to_fold_returns_none() {
        let roles = alternating(20);
        let first = plan_summary(&roles, 0).expect("首次应当要摘要");
        // 已经覆盖到区间末尾：没有新内容，不要再摘要一次。
        assert!(plan_summary(&roles, first.end).is_none());
        assert!(plan_summary(&roles, roles.len()).is_none());
    }

    #[test]
    fn incremental_plan_starts_where_the_last_summary_stopped() {
        let mut roles = alternating(30); // 60 条
        let first = plan_summary(&roles, 0).unwrap();
        assert_eq!(first.start, 0, "首次摘要从最开始折");

        // 只折一次的话，剩下的原文正好是 SUMMARY_KEEP_RECENT 条，没有新内容
        // 可折 —— 这时再摘要一次是**正确的**空操作。
        assert!(
            plan_summary(&roles, first.end).is_none(),
            "剩余原文只够保留，不该再折"
        );

        // 会话继续变长，才有新内容可折。
        roles.extend(alternating(10)); // 再 20 条 → 80 条
        let second = plan_summary(&roles, first.end).expect("新增内容应当触发第二次摘要");
        assert_eq!(second.start, first.end, "第二次应当从上次的终点继续");
        assert!(second.end > first.end, "第二次应当折得更多");
        assert_eq!(roles[second.end], "user");
    }

    #[test]
    fn a_huge_fold_is_shrunk_to_fit_the_prompt_budget() {
        // 60 条、每条 1000 个汉字（≈1000 token）：整段约 6 万 token，
        // 远超 12000 的预算。不收缩的话这个摘要请求会**永远**失败。
        let roles = alternating(30);
        let contents: Vec<String> = (0..60).map(|_| "字".repeat(1_000)).collect();

        let planned = plan_summary(&roles, 0).expect("应当要摘要");
        let shrunk = shrink_to_budget(&roles, &contents, planned.clone(), MAX_SUMMARY_INPUT_TOKENS)
            .expect("应当收缩出一个区间");

        assert_eq!(shrunk.start, planned.start, "起点不能变（覆盖范围要连续）");
        assert!(shrunk.end <= planned.end);
        assert!(shrunk.end > shrunk.start, "不能收缩成空区间");
        assert_eq!(roles[shrunk.end], "user", "边界仍要落在 user 上");

        let used: usize = (shrunk.start..shrunk.end)
            .map(|index| estimate_tokens(&contents[index]))
            .sum();
        assert!(
            used <= MAX_SUMMARY_INPUT_TOKENS,
            "收缩后必须装得下，实际 {used}"
        );
    }

    #[test]
    fn a_fold_that_already_fits_is_left_alone() {
        let roles = alternating(30);
        let contents: Vec<String> = (0..60).map(|_| "短".to_string()).collect();

        let planned = plan_summary(&roles, 0).unwrap();
        let shrunk =
            shrink_to_budget(&roles, &contents, planned.clone(), MAX_SUMMARY_INPUT_TOKENS).unwrap();

        assert_eq!(shrunk, planned, "装得下就别动它");
    }

    #[test]
    fn an_oversized_single_message_still_makes_progress() {
        // 单条就超预算：也必须折掉一条，否则 `covered` 永远不前进、
        // 摘要功能彻底卡死（每轮都重试同一个超限请求）。
        let roles = alternating(30);
        let mut contents: Vec<String> = (0..60).map(|_| "短".to_string()).collect();
        contents[0] = "字".repeat(MAX_SUMMARY_INPUT_TOKENS * 3);

        let planned = plan_summary(&roles, 0).unwrap();
        let shrunk = shrink_to_budget(&roles, &contents, planned, MAX_SUMMARY_INPUT_TOKENS)
            .expect("必须给出一个能前进的区间");

        assert!(shrunk.end > shrunk.start, "至少要折掉一条");
        assert_eq!(roles[shrunk.end], "user");
    }

    #[test]
    fn an_empty_range_yields_nothing() {
        let roles = alternating(30);
        let contents: Vec<String> = (0..60).map(|_| "x".to_string()).collect();
        assert!(shrink_to_budget(&roles, &contents, 5..5, 1_000).is_none());
    }

    #[test]
    fn summary_prompt_carries_previous_summary_and_transcript() {
        let messages = vec![msg("user", "我叫小明"), msg("assistant", "你好小明")];
        let fresh = build_summary_messages("", &messages);
        assert_eq!(fresh.len(), 2);
        assert_eq!(fresh[0].role, "system");
        assert!(fresh[1].content.contains("我叫小明"));
        assert!(fresh[1].content.contains("助手：你好小明"));
        assert!(!fresh[1].content.contains("已有摘要"));

        let merged = build_summary_messages("用户叫小明。", &messages);
        assert!(merged[1].content.contains("【已有摘要】"));
        assert!(merged[1].content.contains("用户叫小明。"));
    }

    #[test]
    fn empty_messages_are_skipped_in_the_transcript() {
        let messages = vec![msg("user", "  "), msg("assistant", "有内容")];
        let built = build_summary_messages("", &messages);
        assert!(!built[1].content.contains("用户："));
        assert!(built[1].content.contains("有内容"));
    }

    #[test]
    fn clean_summary_strips_code_fences_and_truncates() {
        assert_eq!(clean_summary("  要点一  "), "要点一");
        assert_eq!(
            clean_summary("```markdown\n要点一\n要点二\n```"),
            "要点一\n要点二"
        );
        assert_eq!(clean_summary("```\n要点\n```"), "要点");

        let long = "字".repeat(MAX_SUMMARY_CHARS + 500);
        let cleaned = clean_summary(&long);
        assert_eq!(cleaned.chars().count(), MAX_SUMMARY_CHARS + 1);
        assert!(cleaned.ends_with('…'));
    }
}
