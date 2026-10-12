//! BM25 排序。
//!
//! 只用最经典的那个版本（Lucene 的 `BM25Similarity` 同款）：
//!
//! ```text
//! score(q, d) = Σ_t idf(t) · tf · (k1 + 1) / ( tf + k1 · (1 − b + b · dl / avgdl) )
//! idf(t)      = ln(1 + (N − df + 0.5) / (df + 0.5))
//! ```
//!
//! 三件事值得写下来：
//!
//! - **idf 一定是正数**（用了 `ln(1 + …)` 这个变体）。经典的 Robertson-Sparck-Jones
//!   公式在「某个词出现在超过一半文档里」时会给出**负** idf，于是命中该词反而扣分 ——
//!   一个在很多文档里都有的词把结果排序搞反，很难查。Lucene 早就换成了这个变体。
//! - **文档长度要归一化**（`b`）：否则「长文档因为词多而永远排前面」。
//! - **排序必须确定**：分数相同时按 chunk id 兜底。否则同一份数据两次搜索结果顺序
//!   不同 —— 对用户是「这软件时而聪明时而糊涂」，对测试是随机失败。

use std::collections::HashMap;

/// BM25 的词频饱和系数。
pub const K1: f64 = 1.2;
/// BM25 的文档长度归一化系数。
pub const B: f64 = 0.75;

/// `ln(1 + (N − df + 0.5) / (df + 0.5))`，恒为正。
pub fn idf(n_docs: i64, doc_freq: i64) -> f64 {
    let n = n_docs.max(0) as f64;
    let df = doc_freq.clamp(0, n_docs.max(0)) as f64;
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

/// 单个词的贡献。
pub fn term_score(tf: f64, doc_len: f64, avg_len: f64, idf: f64) -> f64 {
    if tf <= 0.0 {
        return 0.0;
    }
    // avg_len 为 0 时（空库）不能除零：退化成不做长度归一化
    let norm = if avg_len > 0.0 {
        doc_len / avg_len
    } else {
        1.0
    };
    let denom = tf + K1 * (1.0 - B + B * norm);
    if denom <= 0.0 {
        return 0.0;
    }
    idf * tf * (K1 + 1.0) / denom
}

/// 一条命中（片段 + 分数）。
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub chunk_id: String,
    pub score: f64,
}

/// 一个词的倒排项：`(chunk_id, tf, 该片段的 token 总数)`。
pub type Posting = (String, i64, i64);

/// 对倒排结果打分并排序。
///
/// `postings` 是「词 → 该词的倒排项」；`doc_freq` 是「词 → 文档频率」；
/// `n_docs` 是片段总数。返回按分数降序的前 `limit` 条。
pub fn rank(
    postings: &HashMap<String, Vec<Posting>>,
    doc_freq: &HashMap<String, i64>,
    n_docs: i64,
    limit: usize,
) -> Vec<Scored> {
    if limit == 0 || n_docs <= 0 {
        return Vec::new();
    }

    // 平均长度：用**所有片段**的 token 数。调用方给不出全部片段时，
    // 退化成「候选片段的平均值」也可以接受（只是归一化略偏）。
    let mut lengths: HashMap<&str, i64> = HashMap::new();
    for items in postings.values() {
        for (chunk_id, _, tokens) in items {
            lengths.insert(chunk_id.as_str(), *tokens);
        }
    }
    let avg_len = if lengths.is_empty() {
        0.0
    } else {
        lengths.values().sum::<i64>() as f64 / lengths.len() as f64
    };

    let mut scores: HashMap<String, f64> = HashMap::new();
    for (term, items) in postings {
        let df = doc_freq.get(term).copied().unwrap_or(items.len() as i64);
        let term_idf = idf(n_docs, df);
        for (chunk_id, tf, tokens) in items {
            let contribution = term_score(*tf as f64, *tokens as f64, avg_len, term_idf);
            if contribution > 0.0 {
                *scores.entry(chunk_id.clone()).or_insert(0.0) += contribution;
            }
        }
    }

    let mut ranked: Vec<Scored> = scores
        .into_iter()
        .map(|(chunk_id, score)| Scored { chunk_id, score })
        .collect();

    // 分数降序；同分按 id 升序 —— 保证结果稳定（见文件头的说明）
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    ranked.truncate(limit);
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的倒排构造输入：`(词, [(片段 id, tf, 片段 token 数)])`。
    /// 起个名字纯粹是因为这一串嵌套写两遍就没人看得懂了。
    type RawPostings<'a> = &'a [(&'a str, &'a [(&'a str, i64, i64)])];

    fn postings(items: RawPostings<'_>) -> HashMap<String, Vec<Posting>> {
        items
            .iter()
            .map(|(term, list)| {
                (
                    term.to_string(),
                    list.iter()
                        .map(|(id, tf, tokens)| (id.to_string(), *tf, *tokens))
                        .collect(),
                )
            })
            .collect()
    }

    fn freqs(items: &[(&str, i64)]) -> HashMap<String, i64> {
        items.iter().map(|(t, df)| (t.to_string(), *df)).collect()
    }

    #[test]
    fn idf_is_always_positive() {
        // 经典 RSJ 公式在 df > N/2 时会给出负值，导致「命中反而扣分」
        for (n, df) in [(10, 1), (10, 5), (10, 9), (10, 10), (1, 1), (0, 0)] {
            assert!(idf(n, df) >= 0.0, "idf({n},{df}) = {}", idf(n, df));
        }
    }

    #[test]
    fn rarer_terms_weigh_more() {
        // 同样 tf 下，出现在更少文档里的词应当贡献更大
        let rare = term_score(1.0, 10.0, 10.0, idf(100, 1));
        let common = term_score(1.0, 10.0, 10.0, idf(100, 90));
        assert!(rare > common, "稀有词应当更重：{rare} vs {common}");
    }

    #[test]
    fn more_occurrences_score_higher_but_with_diminishing_returns() {
        let idf = idf(100, 5);
        let t1 = term_score(1.0, 10.0, 10.0, idf);
        let t2 = term_score(2.0, 10.0, 10.0, idf);
        let t10 = term_score(10.0, 10.0, 10.0, idf);
        assert!(t2 > t1);
        assert!(t10 > t2);
        // 饱和：从 10 到 20 的增量远小于 1 到 2
        let t20 = term_score(20.0, 10.0, 10.0, idf);
        assert!(t20 - t10 < t2 - t1, "词频收益应当递减");
    }

    #[test]
    fn shorter_chunks_win_when_frequency_is_equal() {
        let idf = idf(100, 5);
        let short = term_score(3.0, 20.0, 100.0, idf);
        let long = term_score(3.0, 200.0, 100.0, idf);
        assert!(short > long, "同样命中三次，短片段应当更相关");
    }

    #[test]
    fn ranking_orders_by_score_and_respects_limit() {
        let p = postings(&[("rust", &[("c1", 1, 10), ("c2", 5, 10), ("c3", 2, 10)])]);
        let ranked = rank(&p, &freqs(&[("rust", 3)]), 10, 10);
        assert_eq!(
            ranked
                .iter()
                .map(|s| s.chunk_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c2", "c3", "c1"],
            "应当按 tf 降序"
        );
        assert_eq!(rank(&p, &freqs(&[("rust", 3)]), 10, 2).len(), 2);
    }

    #[test]
    fn multiple_terms_accumulate() {
        // c1 命中两个词，c2 只命中一个 tf 更高的词
        let p = postings(&[
            ("alpha", &[("c1", 2, 10)]),
            ("beta", &[("c1", 2, 10), ("c2", 3, 10)]),
        ]);
        let ranked = rank(&p, &freqs(&[("alpha", 1), ("beta", 2)]), 10, 10);
        // c1 两个词都命中，应当排在 c2 前面
        assert_eq!(ranked[0].chunk_id, "c1");
    }

    #[test]
    fn results_are_deterministic_when_scores_tie() {
        // 同分必须稳定排序，否则「同一份数据两次搜索顺序不同」
        let p = postings(&[("x", &[("c-b", 1, 10), ("c-a", 1, 10), ("c-c", 1, 10)])]);
        let first = rank(&p, &freqs(&[("x", 3)]), 10, 3);
        let second = rank(&p, &freqs(&[("x", 3)]), 10, 3);
        assert_eq!(first, second);
        assert_eq!(
            first
                .iter()
                .map(|s| s.chunk_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c-a", "c-b", "c-c"],
            "同分按 id 升序"
        );
    }

    #[test]
    fn empty_inputs_are_safe() {
        assert!(rank(&HashMap::new(), &HashMap::new(), 0, 10).is_empty());
        assert!(rank(&HashMap::new(), &HashMap::new(), 10, 10).is_empty());
        // limit 为 0
        let p = postings(&[("x", &[("c1", 1, 10)])]);
        assert!(rank(&p, &freqs(&[("x", 1)]), 10, 0).is_empty());
    }

    #[test]
    fn zero_length_average_does_not_divide_by_zero() {
        // 空库（avg_len = 0）不能产生 NaN
        let score = term_score(1.0, 0.0, 0.0, 1.0);
        assert!(score.is_finite(), "得到 {score}");
    }

    #[test]
    fn a_chunk_mentioned_everywhere_scores_lower_than_a_rare_one() {
        // 端到端一点：同一个 tf，稀有词的片段应当胜出
        let p = postings(&[
            ("rare", &[("chunk-rare", 2, 10)]),
            ("common", &[("chunk-common", 2, 10)]),
        ]);
        let ranked = rank(&p, &freqs(&[("rare", 1), ("common", 8)]), 10, 10);
        assert_eq!(ranked[0].chunk_id, "chunk-rare");
    }
}
