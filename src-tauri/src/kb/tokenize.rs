//! 本地知识库的分词。
//!
//! 不引分词库（jieba 之类会带词典与体积），用一套**够用且可解释**的规则：
//!
//! - **汉字**：按**单字 + 相邻双字（bigram）**都入索引。
//!   只入双字是标准做法（Lucene 的 CJKBigramFilter），但那样**单字查询搜不到**
//!   （索引里没有单字）。所以两种都入：召回优先，个人知识库的规模下索引翻倍无所谓。
//! - **拉丁字母 / 数字**：按非字母数字切分并小写；单个字母丢弃（噪音），
//!   同时按 `_` 与**驼峰边界**再切一刀 —— 用户存的往往是代码与文档，
//!   `search_knowledge_base` 应该能靠 `knowledge` 搜到。
//! - 其它字符（标点、emoji、空白）一律当分隔符。

/// 单个 token 的长度上限（字符数）。超长的「一个词」多半是粘贴出来的乱码。
pub const MAX_TOKEN_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Han,
    Latin,
    Other,
}

fn kind_of(ch: char) -> CharKind {
    // 汉字与扩展区大致范围；够覆盖日常中文。日文假名归到 Han 一起处理
    // （同样是「没有空格分隔」的文字，bigram 策略通用）。
    let c = ch as u32;
    let is_han = (0x4E00..=0x9FFF).contains(&c)      // CJK 统一表意
        || (0x3400..=0x4DBF).contains(&c)            // 扩展 A
        || (0xF900..=0xFAFF).contains(&c)            // 兼容表意
        || (0x3040..=0x30FF).contains(&c); // 平假名 / 片假名
    if is_han {
        CharKind::Han
    } else if ch.is_ascii_alphanumeric() {
        CharKind::Latin
    } else {
        CharKind::Other
    }
}

/// 把一个拉丁词再按 `_` / `-` / 驼峰边界切开。
///
/// `searchKnowledgeBase` → `search` / `knowledge` / `base`；
/// `snake_case_name` → `snake` / `case` / `name`。
fn split_latin_word(word: &str, out: &mut Vec<String>) -> Option<()> {
    let mut current = String::new();
    let mut prev_lower_or_digit = false;

    for ch in word.chars() {
        if ch == '_' || ch == '-' || ch == '.' {
            push_latin(&mut current, out);
            prev_lower_or_digit = false;
            continue;
        }
        // 驼峰边界：小写/数字 之后出现大写 → 从这里断开
        if ch.is_uppercase() && prev_lower_or_digit {
            push_latin(&mut current, out);
        }
        prev_lower_or_digit = ch.is_lowercase() || ch.is_ascii_digit();
        current.push(ch);
    }
    push_latin(&mut current, out);
    Some(())
}

fn push_latin(current: &mut String, out: &mut Vec<String>) {
    let token = current.trim().to_lowercase();
    current.clear();
    // 单字母丢弃：`a` / `i` 这类会在几乎每篇文档里出现，只会稀释 BM25 的区分度
    if token.chars().count() >= 2 && token.chars().count() <= MAX_TOKEN_CHARS {
        out.push(token);
    }
}

/// 把文本切成检索用的 token（保序、允许重复 —— 调用方要用重复次数算 tf）。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match kind_of(chars[i]) {
            CharKind::Other => {
                i += 1;
            }
            CharKind::Latin => {
                let start = i;
                while i < chars.len() && kind_of(chars[i]) == CharKind::Latin {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let _ = split_latin_word(&word, &mut out);
            }
            CharKind::Han => {
                let start = i;
                while i < chars.len() && kind_of(chars[i]) == CharKind::Han {
                    i += 1;
                }
                let run = &chars[start..i];
                // 单字：让「书」这种一字查询也能命中
                for ch in run {
                    out.push(ch.to_string());
                }
                // 双字：主要区分度来自这里
                for pair in run.windows(2) {
                    if pair[0..2].len() == 2 {
                        out.push(pair.iter().collect::<String>());
                    }
                }
            }
        }
    }

    out
}

/// 查询用的 token：**去重并保序**。
///
/// 为什么与 `tokenize` 分开：索引要 tf（重复次数有意义），而查询里同一个词
/// 出现两次不代表「更重要两倍」—— 按查询词频加权会让重复敲一个词变成一种
/// 粗糙的加权手段，结果不可预期。这里直接去重。
pub fn query_terms(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in tokenize(text) {
        if seen.insert(token.clone()) {
            out.push(token);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_produces_unigrams_and_bigrams() {
        let tokens = tokenize("浮语助手");
        for expected in ["浮", "语", "助", "手", "浮语", "语助", "助手"] {
            assert!(
                tokens.contains(&expected.to_string()),
                "缺少 {expected}：{tokens:?}"
            );
        }
    }

    #[test]
    fn a_single_chinese_char_is_searchable() {
        // 只入 bigram 的实现在这里会返回空 —— 单字查询搜不到是很常见的抱怨
        assert_eq!(tokenize("书"), vec!["书".to_string()]);
        assert!(tokenize("一本书").contains(&"书".to_string()));
    }

    #[test]
    fn chinese_runs_are_split_by_punctuation() {
        // 「你好，世界」不应当产生跨标点的 bigram「好世」
        let tokens = tokenize("你好，世界");
        assert!(tokens.contains(&"你好".to_string()));
        assert!(tokens.contains(&"世界".to_string()));
        assert!(
            !tokens.contains(&"好世".to_string()),
            "不该跨标点组词：{tokens:?}"
        );
    }

    #[test]
    fn latin_is_lowercased_and_split_on_punctuation() {
        let tokens = tokenize("Hello, WORLD! foo-bar");
        for expected in ["hello", "world", "foo", "bar"] {
            assert!(
                tokens.contains(&expected.to_string()),
                "缺少 {expected}：{tokens:?}"
            );
        }
    }

    #[test]
    fn camel_case_and_snake_case_are_split() {
        // 用户存的多半是代码与文档，这些词应当能被单段搜到
        let camel = tokenize("searchKnowledgeBase");
        for expected in ["search", "knowledge", "base"] {
            assert!(
                camel.contains(&expected.to_string()),
                "缺少 {expected}：{camel:?}"
            );
        }
        let snake = tokenize("read_clipboard_text");
        for expected in ["read", "clipboard", "text"] {
            assert!(
                snake.contains(&expected.to_string()),
                "缺少 {expected}：{snake:?}"
            );
        }
    }

    #[test]
    fn single_latin_letters_are_dropped() {
        let tokens = tokenize("a b c ab");
        assert_eq!(tokens, vec!["ab".to_string()]);
    }

    #[test]
    fn mixed_text_keeps_both_sides() {
        let tokens = tokenize("用 Rust 写 BM25 检索");
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"bm25".to_string()));
        assert!(tokens.contains(&"检索".to_string()));
        assert!(tokens.contains(&"检".to_string()));
    }

    #[test]
    fn punctuation_digits_and_emoji_do_not_crash() {
        for text in ["", "   ", "!!!", "🌊🌊", "\u{0}", "--", "..."] {
            let tokens = tokenize(text);
            assert!(
                tokens.is_empty() || tokens.iter().all(|t| !t.is_empty()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn overlong_tokens_are_dropped() {
        let long = "x".repeat(MAX_TOKEN_CHARS + 1);
        assert!(tokenize(&long).is_empty());
        let ok = "x".repeat(MAX_TOKEN_CHARS);
        assert_eq!(tokenize(&ok).len(), 1);
    }

    #[test]
    fn query_terms_are_deduplicated_but_keep_order() {
        // 注意：中间的空格会把汉字串切断，所以**不会**产生跨空格的 bigram「语助」。
        // 这是有意的 —— 空格在中文里通常是真实的分隔（用户不会平白敲一个空格）。
        assert_eq!(
            query_terms("浮语 浮语 助手"),
            vec!["浮", "语", "浮语", "助", "手", "助手"]
        );
        // 重复出现不应当让某个词在结果里出现两次
        let terms = query_terms("rust rust rust");
        assert_eq!(terms, vec!["rust".to_string()]);
    }

    #[test]
    fn a_space_breaks_chinese_bigrams() {
        // 连在一起的「浮语助手」会产生 语助，而「浮语 助手」不会 ——
        // 两种情况都能召回，但索引里的词不同
        assert!(tokenize("浮语助手").contains(&"语助".to_string()));
        assert!(!tokenize("浮语 助手").contains(&"语助".to_string()));
    }
}
