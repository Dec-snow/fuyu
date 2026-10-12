//! 本地知识库：把文档切块、建倒排索引、用 BM25 检索。
//!
//! 三个子模块都是**纯函数**（分词 / 切块 / 打分），可以脱离数据库单测；
//! SQL 部分在 [`crate::storage::database`]，命令与工具入口在
//! [`crate::commands::kb`] 与 [`crate::tools::registry`]。
//!
//! 为什么自己实现 BM25 而不是直接用 SQLite 的 FTS5：
//! - 消息搜索用的是 FTS5 的 **trigram** 分词器（为了中文子串匹配），
//!   而 trigram 下 `bm25()` 的排序基本没有区分度（三元组遍地都是）；
//! - 真正的中文 BM25 需要「词」的概念，而 FTS5 的 `unicode61` 不切中文。
//!   自己实现分词 + BM25 反而更可控，而且能单测（见 [`rank`] 的测试）。

pub mod chunk;
pub mod rank;
pub mod tokenize;

pub use chunk::{chunk_text, DEFAULT_CHUNK_CHARS, DEFAULT_OVERLAP_CHARS};
pub use rank::rank;
pub use tokenize::{query_terms, tokenize};

/// 一次检索默认返回几个片段。
pub const DEFAULT_SEARCH_LIMIT: usize = 5;
/// 检索返回条数的上限（工具调用参数不可信）。
pub const MAX_SEARCH_LIMIT: usize = 20;
/// 单篇文档的字符数上限（超过就拒收，避免一次导入把库撑爆）。
pub const MAX_DOCUMENT_CHARS: usize = 2_000_000;
/// 一个片段最多取多少个 token 参与打分（超长片段截断，防止单条 SQL 行过大）。
pub const MAX_TOKENS_PER_CHUNK: usize = 4_000;

/// 知识库里的文档（列表用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbDocument {
    pub id: String,
    pub title: String,
    /// 来源：文件名，或「粘贴的文本」。
    pub source: String,
    pub chars: i64,
    pub chunk_count: i64,
    pub added_at: i64,
}

/// 一次检索的命中。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbHit {
    pub chunk_id: String,
    pub document_id: String,
    pub title: String,
    /// 这是文档里的第几片（从 1 开始，给人看的位置感）。
    pub ordinal: i64,
    pub text: String,
    pub score: f64,
}

/// 知识库概览。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbStats {
    pub documents: i64,
    pub chunks: i64,
    pub terms: i64,
    pub chars: i64,
}
