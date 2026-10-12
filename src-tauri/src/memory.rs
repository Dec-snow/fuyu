//! 长期记忆的**纯逻辑**：校验、去重、相关性排序、提示词拼装。
//!
//! 与知识库的分工：知识库是「用户主动导入的资料」，记忆是「从对话里沉淀下来的
//! 关于用户的事实」。两者的检索需求完全不同 ——
//!
//! **记忆刻意不做倒排索引**。一条记忆就是一句话，总量在几百条量级，
//! 直接遍历 + 现算 BM25 的代价可以忽略；而维护倒排表要处理 df 增减、
//! 删除时重算、切片与原文的对应……那些复杂度只有在「上万段文本」时才划算
//! （知识库正是那种规模）。**索引的价值随语料规模增长，小语料上它只是债。**
//!
//! 三件事必须有，否则记忆库会越用越脏：
//! - **长度上限**：一条记忆该是一句话，不是一段日记；
//! - **近似去重**：不然「用户喜欢 Rust」会攒成二十条，把提示词挤满；
//! - **相关性挑选**：只把与本次提问有关的几条拼进去，而不是全部。

use crate::errors::{AppError, AppResult};
use crate::kb::rank::{idf, term_score};

/// 单条记忆的字符上限。
pub const MAX_MEMORY_CHARS: usize = 500;
/// 记忆条数上限。
pub const MAX_MEMORIES: i64 = 500;
/// 每次注入最多几条。
pub const INJECT_LIMIT: usize = 5;
/// 相似度超过这个值就当作同一条。
pub const DEDUP_THRESHOLD: f64 = 0.7;
/// 至少要共享这么多个 token 才算重复（挡住「短句是长句子集」的误判）。
pub const MIN_SHARED_TOKENS: usize = 3;

/// 一条记忆。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    pub text: String,
    /// 来源：`tool`（模型自己记的）/ `user`（手动添加）。
    pub source: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    /// 被注入过几次（界面用它显示「用过 N 次」，用户可以据此判断有没有用）。
    pub used_count: i64,
}

/// 两条记忆的相似度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    /// 重叠系数：`|A ∩ B| / min(|A|, |B|)`。
    pub overlap: f64,
    /// 共享的 token 个数（用来挡住「短句命中长句」的误判）。
    pub shared: usize,
}

/// 计算相似度。
///
/// ⚠️ 这里原本用的是 **Jaccard**（`|A ∩ B| / |A ∪ B|`），被测试打回来了：
/// 「用户喜欢用 Rust 写程序」和「用户喜欢用 Rust 写代码」是**明显该合并的重复**，
/// 但在这套「单字 + 双字」分词下 Jaccard 只有约 0.58 —— 因为每种写法都会产生
/// 十几个不同的字/双字 token，分母被撑大了。
///
/// 换成**重叠系数**（除以较小的那个集合）：它衡量的是「短的那条是不是基本被
/// 长的那条包含」，这正是「同一个意思换个说法」的形态。中文里换说法往往就是
/// 加几个字或换一个词，重叠系数能抓住，Jaccard 抓不住。
///
/// 代价是「短句是长句子集」时会偏高（例如「Rust」与「用户喜欢用 Rust 写程序」），
/// 所以判定重复时必须**同时**要求共享 token 数达到 [`MIN_SHARED_TOKENS`]。
pub fn similarity(a: &[String], b: &[String]) -> Similarity {
    if a.is_empty() || b.is_empty() {
        // 两边都空视为相同；一边空一边不空则是 0（但共享数不足，会被判为不重复）
        let both_empty = a.is_empty() && b.is_empty();
        return Similarity {
            overlap: if both_empty { 1.0 } else { 0.0 },
            shared: 0,
        };
    }

    let set_a: std::collections::HashSet<&String> = a.iter().collect();
    let set_b: std::collections::HashSet<&String> = b.iter().collect();
    let shared = set_a.intersection(&set_b).count();
    let smaller = set_a.len().min(set_b.len());

    Similarity {
        overlap: if smaller == 0 {
            0.0
        } else {
            shared as f64 / smaller as f64
        },
        shared,
    }
}

/// 这两条记忆是不是同一件事（可以合并）。
pub fn is_duplicate(a: &[String], b: &[String]) -> bool {
    let s = similarity(a, b);
    s.shared >= MIN_SHARED_TOKENS && s.overlap >= DEDUP_THRESHOLD
}

/// 在已有记忆里找一条「和新候选几乎一样」的，返回它的 id。
///
/// 命中时调用方的做法是**更新那一条**而不是新增 —— 这正是防止越攒越脏的关键。
pub fn find_near_duplicate<'a>(
    existing: &'a [(String, Vec<String>)],
    candidate: &[String],
) -> Option<&'a str> {
    let mut best: Option<(&str, f64)> = None;
    for (id, tokens) in existing {
        // 复用 `is_duplicate`，免得阈值判断写两遍、改一处漏一处
        if !is_duplicate(tokens, candidate) {
            continue;
        }
        let overlap = similarity(tokens, candidate).overlap;
        if best.is_none_or(|(_, b)| overlap > b) {
            best = Some((id.as_str(), overlap));
        }
    }
    best.map(|(id, _)| id)
}

/// 收录一条记忆的结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAddOutcome {
    pub memory: Memory,
    /// 是不是**合并**进了已有的一条（而不是新增）。界面据此显示「已更新」。
    pub merged: bool,
}

/// 收录前的校验与规整。
///
/// 多行会被压成一行：记忆是要拼进系统提示词的一段列表项，换行会把列表打乱。
pub fn validate(text: &str) -> AppResult<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return Err(AppError::invalid("记忆内容不能为空"));
    }
    let chars = flat.chars().count();
    if chars > MAX_MEMORY_CHARS {
        return Err(AppError::invalid(format!(
            "这条记忆太长（{chars} 字，上限 {MAX_MEMORY_CHARS} 字）—— 记忆应当是一句话"
        )));
    }
    Ok(flat)
}

/// 挑出与查询最相关的若干条记忆。
///
/// 复用知识库那套 BM25 打分（[`crate::kb::rank`]），只是语料换成「每条记忆一句话」：
/// df 与平均长度都在这里现算。返回按分数降序的 `(id, score)`，**不计分的不返回**
/// （相关性为 0 的记忆拼进提示词只会干扰模型）。
pub fn rank_relevant(
    memories: &[(String, Vec<String>)],
    query_terms: &[String],
    limit: usize,
) -> Vec<(String, f64)> {
    if limit == 0 || memories.is_empty() || query_terms.is_empty() {
        return Vec::new();
    }

    let n_docs = memories.len() as f64;
    let avg_len = memories
        .iter()
        .map(|(_, tokens)| tokens.len() as f64)
        .sum::<f64>()
        / n_docs;

    // 查询词在语料里的文档频率
    let df: Vec<f64> = query_terms
        .iter()
        .map(|term| {
            memories
                .iter()
                .filter(|(_, tokens)| tokens.iter().any(|t| t == term))
                .count() as f64
        })
        .collect();

    let mut scored: Vec<(String, f64)> = Vec::new();
    for (id, tokens) in memories {
        let doc_len = tokens.len() as f64;
        let mut score = 0.0;
        for (index, term) in query_terms.iter().enumerate() {
            let tf = tokens.iter().filter(|t| *t == term).count() as f64;
            if tf <= 0.0 {
                continue;
            }
            let term_idf = idf(n_docs as i64, df[index] as i64);
            score += term_score(tf, doc_len, avg_len, term_idf);
        }
        if score > 0.0 {
            scored.push((id.clone(), score));
        }
    }

    // 分数降序；同分按 id 升序，保证结果稳定
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    scored.truncate(limit);
    scored
}

/// 把挑出来的记忆拼成一段系统提示词。
///
/// 两点是刻意写进去的：
/// - **「不确定就别用」**：记忆会过时（用户换了技术栈、项目做完了）。
///   不提醒的话，模型倾向于把记忆当成硬事实，说出「你之前说你喜欢 X」这种
///   已经不对的话；
/// - **不说「记忆」这个词**：告诉模型「以下是关于用户的背景」，比
///   「以下是你的记忆」更像正常的背景信息，模型也更不容易在回答里
///   提「根据我的记忆」这种让用户发毛的话。
pub fn prompt_block(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "以下是之前对话里记下的、关于用户的背景信息，供参考（可能已经过时，不确定就不要用，也不要主动提起这些信息的来源）：",
    );
    for memory in memories {
        out.push_str("\n- ");
        out.push_str(memory.text.trim());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::tokenize;

    fn mem(id: &str, text: &str) -> Memory {
        Memory {
            id: id.to_string(),
            text: text.to_string(),
            source: "tool".to_string(),
            created_at: 0,
            last_used_at: None,
            used_count: 0,
        }
    }

    fn corpus(items: &[(&str, &str)]) -> Vec<(String, Vec<String>)> {
        items
            .iter()
            .map(|(id, text)| (id.to_string(), tokenize(text)))
            .collect()
    }

    #[test]
    fn validate_trims_and_flattens_to_one_line() {
        assert_eq!(validate("  用户喜欢  Rust  ").unwrap(), "用户喜欢 Rust");
        // 多行会被压成一行：记忆是提示词里的一个列表项
        assert_eq!(validate("第一行\n第二行").unwrap(), "第一行 第二行");
    }

    #[test]
    fn validate_rejects_empty_and_overlong() {
        assert!(validate("").is_err());
        assert!(validate("   \n  ").is_err());
        let long = "啊".repeat(MAX_MEMORY_CHARS + 1);
        let err = validate(&long).unwrap_err();
        assert!(err.message.contains("太长"), "{}", err.message);
        // 边界值本身要能通过
        assert!(validate(&"啊".repeat(MAX_MEMORY_CHARS)).is_ok());
    }

    #[test]
    fn similarity_is_one_for_identical_and_zero_for_disjoint() {
        let a = tokenize("用户喜欢 Rust");
        let b = tokenize("用户喜欢 Rust");
        let c = tokenize("今天天气不错");
        assert_eq!(similarity(&a, &b).overlap, 1.0);
        assert_eq!(similarity(&a, &c).overlap, 0.0);
        assert!(is_duplicate(&a, &b));
        assert!(!is_duplicate(&a, &c));
    }

    #[test]
    fn similarity_catches_reworded_duplicates() {
        // 这是中文的实际场景：同一个意思换个说法，字面差别很大但词几乎一样。
        // 用 Jaccard 时这里只有约 0.58（分母被两种写法各自的 token 撑大了），
        // 换成重叠系数才抓得住 —— 见 `similarity` 的注释。
        let a = tokenize("用户喜欢用 Rust 写程序");
        let b = tokenize("用户喜欢用 Rust 写代码");
        let s = similarity(&a, &b);
        assert!(
            s.overlap >= DEDUP_THRESHOLD && s.shared >= MIN_SHARED_TOKENS,
            "换个说法应当被判为重复：overlap={}, shared={}",
            s.overlap,
            s.shared
        );
        assert!(is_duplicate(&a, &b));
    }

    #[test]
    fn a_short_memory_is_not_swallowed_by_a_longer_one() {
        // 重叠系数的已知弱点：短句容易被长句「包含」。所以还要求共享词数达标。
        let short = tokenize("Rust");
        let long = tokenize("用户喜欢用 Rust 写桌面应用并且用它做后端服务");
        let s = similarity(&short, &long);
        assert_eq!(s.overlap, 1.0, "短句确实被完全包含");
        assert!(s.shared < MIN_SHARED_TOKENS);
        assert!(!is_duplicate(&short, &long), "共享词太少，不该判成重复");
    }

    #[test]
    fn empty_token_sets_do_not_panic() {
        // 注意用**能被分词器留下**的 token：单字母 `x` 会被丢掉
        let real = tokenize("浮语");
        assert_eq!(similarity(&[], &[]).overlap, 1.0);
        assert_eq!(similarity(&[], &real).overlap, 0.0);
        assert!(!is_duplicate(&[], &real));
    }

    #[test]
    fn find_near_duplicate_picks_the_closest_one() {
        let existing = corpus(&[
            ("m1", "用户喜欢用 Rust 写程序"),
            ("m2", "用户在做一个桌面应用"),
        ]);
        let candidate = tokenize("用户喜欢用 Rust 写代码");
        assert_eq!(find_near_duplicate(&existing, &candidate), Some("m1"));

        // 完全无关的新内容不该被误判成重复
        let fresh = tokenize("用户住在杭州");
        assert_eq!(find_near_duplicate(&existing, &fresh), None);
    }

    #[test]
    fn find_near_duplicate_on_empty_corpus_is_none() {
        assert_eq!(find_near_duplicate(&[], &tokenize("随便")), None);
    }

    #[test]
    fn rank_relevant_returns_the_matching_memories_first() {
        let memories = corpus(&[
            ("m1", "用户喜欢用 Rust 写程序"),
            ("m2", "用户在做桌面应用"),
            ("m3", "用户的猫叫团子"),
        ]);
        let ranked = rank_relevant(&memories, &tokenize("Rust"), 5);
        assert_eq!(ranked.len(), 1, "只有 m1 与 Rust 相关：{ranked:?}");
        assert_eq!(ranked[0].0, "m1");
        assert!(ranked[0].1 > 0.0);
    }

    #[test]
    fn rank_relevant_excludes_irrelevant_and_respects_limit() {
        let memories = corpus(&[
            ("m1", "用户喜欢 Rust"),
            ("m2", "用户喜欢 Rust 和 Tauri"),
            ("m3", "用户喜欢 Rust 写后端"),
        ]);
        // 三条都含 Rust → 都相关，但 limit 要生效
        let ranked = rank_relevant(&memories, &tokenize("Rust"), 2);
        assert_eq!(ranked.len(), 2);

        // 完全不相关的查询返回空
        assert!(rank_relevant(&memories, &tokenize("量子力学"), 5).is_empty());
    }

    #[test]
    fn rank_relevant_is_deterministic_and_safe_on_empty_inputs() {
        let memories = corpus(&[("m1", "用户喜欢 Rust"), ("m2", "用户喜欢 Rust")]);
        let first = rank_relevant(&memories, &tokenize("Rust"), 5);
        let second = rank_relevant(&memories, &tokenize("Rust"), 5);
        assert_eq!(first, second);
        // 同分时按 id 升序
        assert_eq!(first[0].0, "m1");

        assert!(rank_relevant(&[], &tokenize("Rust"), 5).is_empty());
        assert!(rank_relevant(&memories, &[], 5).is_empty());
        assert!(rank_relevant(&memories, &tokenize("Rust"), 0).is_empty());
    }

    #[test]
    fn prompt_block_lists_the_memories_with_a_caution() {
        let block = prompt_block(&[mem("m1", "用户喜欢 Rust"), mem("m2", "用户在做桌面应用")]);
        assert!(
            block.contains("可能已经过时"),
            "必须提醒记忆会过时：{block}"
        );
        assert!(block.contains("- 用户喜欢 Rust"));
        assert!(block.contains("- 用户在做桌面应用"));
        // 不应当出现「记忆」这个词（见函数注释）
        assert!(
            !block.contains("记忆"),
            "不该让模型说「根据我的记忆」：{block}"
        );
    }

    #[test]
    fn prompt_block_is_empty_for_no_memories() {
        // 空块不能被拼进系统提示词（会平白多出一段空说明）
        assert!(prompt_block(&[]).is_empty());
    }
}
