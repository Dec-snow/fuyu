//! Agent 侧的能力：上下文预算（含滚动摘要的装配）与滚动摘要的规划。
//!
//! 参考项目在这一层还有知识库检索、长期记忆、技能注入 —— 那些见
//! `docs/analysis/05-Agent与工具系统分析.md`。

pub mod context;
pub mod summary;
