//! 费用估算。
//!
//! **这一整块的核心设计只有一条：估不出来就说估不出来。**
//!
//! 价格表是内置的、会过时（各家一年能改好几次），而且用户完全可以自己加一个
//! 从没见过的模型。所以：
//! - 表里没有的模型返回 `None`，调用方必须把它列进 `unpriced` 里显示出来；
//! - 悄悄按 0 计入会给出一个**看起来很确定的错数字** —— 那比不给数字更糟，
//!   因为用户会拿它去对账。
//!
//! 单位：人民币元 / 百万 token（界面会注明「估算，仅供参考」）。

use serde::Serialize;

/// 一个模型的价格（元 / 百万 token）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl Price {
    /// 估算 `input` / `output` 个 token 的花费（元）。
    pub fn cost(&self, input: i64, output: i64) -> f64 {
        let i = (input.max(0) as f64) / 1_000_000.0 * self.input_per_million;
        let o = (output.max(0) as f64) / 1_000_000.0 * self.output_per_million;
        i + o
    }
}

/// 内置价格表（元 / 百万 token，2026 年前后的公开报价，**会过时**）。
///
/// 匹配用的是「模型 key 的小写子串」，从**更具体**到更宽泛依次判断：
/// `deepseek-reasoner` 必须排在 `deepseek` 前面，否则推理模型会被按对话模型
/// 的价格算，而它贵好几倍 —— 这种错误不会有任何提示，只会让估算偏低。
fn table() -> &'static [(&'static str, f64, f64)] {
    &[
        // DeepSeek
        ("deepseek-reasoner", 4.0, 16.0),
        ("deepseek-r1", 4.0, 16.0),
        ("deepseek-chat", 2.0, 8.0),
        ("deepseek", 2.0, 8.0),
        // OpenAI
        ("gpt-4o-mini", 1.1, 4.4),
        ("gpt-4o", 18.0, 72.0),
        ("gpt-4.1-mini", 3.0, 12.0),
        ("gpt-4.1", 15.0, 60.0),
        ("gpt-4-turbo", 72.0, 216.0),
        ("gpt-3.5", 3.6, 10.8),
        ("o1-mini", 8.0, 32.0),
        ("o1", 108.0, 432.0),
        ("o3-mini", 8.0, 32.0),
        // Anthropic
        ("claude-3-5-haiku", 5.8, 29.0),
        ("claude-3-5-sonnet", 21.6, 108.0),
        ("claude-3-7-sonnet", 21.6, 108.0),
        ("claude-sonnet", 21.6, 108.0),
        ("claude-opus", 108.0, 540.0),
        ("claude-haiku", 5.8, 29.0),
        // Google
        ("gemini-2.0-flash", 0.7, 2.9),
        ("gemini-1.5-flash", 0.5, 2.2),
        ("gemini-1.5-pro", 9.0, 36.0),
        ("gemini", 0.7, 2.9),
        // 国内其它
        ("qwen-max", 20.0, 60.0),
        ("qwen-plus", 0.8, 2.0),
        ("qwen", 0.8, 2.0),
        ("glm-4-plus", 50.0, 50.0),
        ("glm-4", 1.0, 1.0),
        ("moonshot-v1-128k", 60.0, 60.0),
        ("moonshot-v1-32k", 24.0, 24.0),
        ("moonshot", 12.0, 12.0),
        ("kimi", 12.0, 12.0),
        ("ernie", 4.0, 8.0),
        ("hunyuan", 4.0, 8.0),
        ("spark", 2.0, 2.0),
        ("doubao", 0.8, 2.0),
        ("abab", 1.0, 1.0),
    ]
}

/// 查一个模型的价格；**查不到返回 `None`**（不要瞎猜）。
///
/// `provider_id` 目前不参与匹配：同一个模型在不同服务商下的报价确实不同，
/// 但内置预置的服务商里同名模型的差异远小于「认不出来」的代价。
/// 留着这个参数是为了将来能按服务商细分。
pub fn builtin_price(_provider_id: &str, model_key: &str) -> Option<Price> {
    let key = model_key.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    // 表本身就是按「具体 → 宽泛」排的，第一个命中的就是最合适的。
    table()
        .iter()
        .find(|(needle, _, _)| key.contains(needle))
        .map(|(_, input, output)| Price {
            input_per_million: *input,
            output_per_million: *output,
        })
}

/// 一行用量（按天 + 模型聚合）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    /// `YYYY-MM-DD`（本地时间）。
    pub day: String,
    pub provider_id: String,
    pub model_key: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub messages: i64,
}

/// 一天的汇总（界面按天画图用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDay {
    pub day: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub messages: i64,
    /// 这一天里**有价格**的那些模型的花费合计（元）。
    pub cost: f64,
    /// 这一天是否含有「价格表里没有」的模型 —— 有的话 `cost` 是**偏低的**，
    /// 界面必须说明，不能把偏低的数字当成全部。
    pub has_unpriced: bool,
}

/// 估算结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub days: Vec<UsageDay>,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_messages: i64,
    /// 有价格部分的估算花费（元）。
    pub estimated_cost: f64,
    /// 价格表里认不出来的模型（去重、稳定排序）。
    pub unpriced_models: Vec<String>,
    /// 认出来的模型数（用来判断「一个都没认出来」这种全表失效的情况）。
    pub priced_models: usize,
}

/// 把逐行用量汇总成按天的报告。
///
/// 纯函数：不碰数据库、不碰时间，方便单测（尤其是「认不出来的模型怎么处理」
/// 这条 —— 它决定了界面上的数字可不可信）。
pub fn summarize(rows: &[UsageRow]) -> UsageReport {
    use std::collections::BTreeMap;

    let mut by_day: BTreeMap<String, UsageDay> = BTreeMap::new();
    let mut unpriced: Vec<String> = Vec::new();
    let mut priced_models: Vec<String> = Vec::new();

    for row in rows {
        let price = builtin_price(&row.provider_id, &row.model_key);
        match price {
            Some(_) => {
                if !priced_models.contains(&row.model_key) {
                    priced_models.push(row.model_key.clone());
                }
            }
            None => {
                if !unpriced.contains(&row.model_key) {
                    unpriced.push(row.model_key.clone());
                }
            }
        }

        let entry = by_day.entry(row.day.clone()).or_insert_with(|| UsageDay {
            day: row.day.clone(),
            input_tokens: 0,
            output_tokens: 0,
            messages: 0,
            cost: 0.0,
            has_unpriced: false,
        });

        entry.input_tokens += row.input_tokens;
        entry.output_tokens += row.output_tokens;
        entry.messages += row.messages;
        match price {
            Some(p) => entry.cost += p.cost(row.input_tokens, row.output_tokens),
            None => entry.has_unpriced = true,
        }
    }

    // 最新的在前（界面直接从上往下画）。
    let days: Vec<UsageDay> = by_day.into_values().rev().collect();
    unpriced.sort();

    UsageReport {
        total_input_tokens: days.iter().map(|d| d.input_tokens).sum(),
        total_output_tokens: days.iter().map(|d| d.output_tokens).sum(),
        total_messages: days.iter().map(|d| d.messages).sum(),
        estimated_cost: days.iter().map(|d| d.cost).sum(),
        unpriced_models: unpriced,
        priced_models: priced_models.len(),
        days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(day: &str, model: &str, input: i64, output: i64) -> UsageRow {
        UsageRow {
            day: day.to_string(),
            provider_id: "p".to_string(),
            model_key: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            messages: 1,
        }
    }

    #[test]
    fn a_million_tokens_costs_exactly_the_listed_price() {
        let price = builtin_price("deepseek", "deepseek-chat").unwrap();
        assert_eq!(price.cost(1_000_000, 0), price.input_per_million);
        assert_eq!(price.cost(0, 1_000_000), price.output_per_million);
        assert_eq!(price.cost(0, 0), 0.0);
        // 半个百万 = 一半的价格
        assert_eq!(price.cost(500_000, 0), price.input_per_million / 2.0);
    }

    #[test]
    fn negative_token_counts_do_not_produce_negative_cost() {
        // 数据库里不该出现负数，但估算函数不能因此给出「负花费」这种鬼数字。
        let price = builtin_price("p", "deepseek-chat").unwrap();
        assert_eq!(price.cost(-100, -100), 0.0);
    }

    #[test]
    fn the_more_specific_model_wins() {
        // 这一条是这张表最容易写错的地方：deepseek-reasoner 比 deepseek-chat
        // 贵得多。如果匹配顺序写反了，估算会**偏低**而且毫无提示。
        let reasoner = builtin_price("deepseek", "deepseek-reasoner").unwrap();
        let chat = builtin_price("deepseek", "deepseek-chat").unwrap();
        assert!(
            reasoner.input_per_million > chat.input_per_million,
            "推理模型不能被按对话模型计价"
        );
        assert_eq!(reasoner.input_per_million, 4.0);
    }

    #[test]
    fn matching_ignores_case_and_surrounding_space() {
        assert!(builtin_price("p", "  DeepSeek-Chat  ").is_some());
        assert!(builtin_price("p", "GPT-4O-MINI").is_some());
    }

    #[test]
    fn an_unknown_model_is_not_guessed() {
        // 这是整块的核心：认不出来就必须返回 None，让调用方显示「未计入」。
        assert!(builtin_price("p", "my-local-llama-3").is_none());
        assert!(builtin_price("p", "").is_none());
        assert!(builtin_price("p", "   ").is_none());
    }

    #[test]
    fn summarize_adds_up_days_and_reports_unpriced_models() {
        let rows = vec![
            row("2026-10-11", "deepseek-chat", 1_000_000, 0),
            row("2026-10-11", "deepseek-chat", 0, 1_000_000),
            row("2026-10-10", "my-local-llama-3", 999_999, 999_999),
        ];
        let report = summarize(&rows);

        assert_eq!(report.days.len(), 2);
        // 最新在前
        assert_eq!(report.days[0].day, "2026-10-11");
        assert_eq!(report.days[1].day, "2026-10-10");

        assert_eq!(report.total_input_tokens, 1_000_000 + 999_999);
        assert_eq!(report.total_output_tokens, 1_000_000 + 999_999);
        assert_eq!(report.total_messages, 3);

        // 只有认得出的那一行计入花费
        assert_eq!(report.estimated_cost, 2.0 + 8.0);
        assert_eq!(report.days[0].cost, 10.0);
        assert_eq!(report.days[1].cost, 0.0);

        // 但「有没算进去的」必须被标出来，否则那个数字会被当成全部
        assert!(report.days[1].has_unpriced);
        assert_eq!(report.unpriced_models, vec!["my-local-llama-3"]);
        assert_eq!(report.priced_models, 1);
    }

    #[test]
    fn a_report_with_no_rows_is_empty_not_a_panic() {
        let report = summarize(&[]);
        assert!(report.days.is_empty());
        assert_eq!(report.estimated_cost, 0.0);
        assert_eq!(report.total_messages, 0);
        assert!(report.unpriced_models.is_empty());
    }

    #[test]
    fn unpriced_models_are_listed_once_even_across_many_days() {
        let rows = vec![
            row("2026-10-11", "unknown-a", 1, 1),
            row("2026-10-10", "unknown-a", 1, 1),
            row("2026-10-09", "unknown-a", 1, 1),
        ];
        let report = summarize(&rows);
        assert_eq!(report.unpriced_models, vec!["unknown-a"]);
        assert!(report.days.iter().all(|d| d.has_unpriced));
    }
}
