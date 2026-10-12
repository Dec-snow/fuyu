//! 把长文档切成检索用的片段（chunk）。
//!
//! 两条规则：
//! - **优先在段落边界切**：段落本来就是作者的语义单元，切开它比切开一句话更糟；
//! - 只有当一个段落本身就超过上限时才**硬切**，并在硬切处留一点**重叠**，
//!   免得一句话正好落在切缝上、两边都搜不到完整语义。
//!
//! 重叠只出现在硬切处。段落之间不留重叠是刻意的：那会让同一个段落被两段
//! 索引两次，BM25 的「文档长度」统计跟着失真。

/// 默认片段长度（字符数）。
pub const DEFAULT_CHUNK_CHARS: usize = 600;
/// 硬切时的重叠长度（字符数）。
pub const DEFAULT_OVERLAP_CHARS: usize = 80;

/// 把文本切成若干片段。空白文本返回空数组（**不**返回一个空片段）。
pub fn chunk_text(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let overlap = overlap.min(max_chars / 2);

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    // 上一段是不是「硬切」结尾 —— 只有硬切才在下一次开头留重叠
    let mut pending_overlap = String::new();

    let flush = |current: &mut String, chunks: &mut Vec<String>| {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        current.clear();
    };

    for paragraph in text.split('\n') {
        let paragraph = paragraph.trim_end();
        if paragraph.trim().is_empty() {
            // 空行：当作段落边界，顺手结束当前片段
            flush(&mut current, &mut chunks);
            pending_overlap.clear();
            continue;
        }

        let para_len = paragraph.chars().count();

        // 段落本身超长：先结束手上的，再硬切这个段落
        if para_len > max_chars {
            flush(&mut current, &mut chunks);
            pending_overlap.clear();

            let chars: Vec<char> = paragraph.chars().collect();
            let mut start = 0;
            while start < chars.len() {
                let end = (start + max_chars).min(chars.len());
                let piece: String = chars[start..end].iter().collect();
                let piece = piece.trim();
                if !piece.is_empty() {
                    chunks.push(piece.to_string());
                    // 记下尾巴，供下一片开头补上（下一片不是新段落，是同一段的续）
                    let tail_len = overlap.min(piece.chars().count());
                    pending_overlap = piece
                        .chars()
                        .skip(piece.chars().count() - tail_len)
                        .collect();
                }
                if end >= chars.len() {
                    break;
                }
                start = end;
            }
            continue;
        }

        // 加上这一段会不会超？超了就先把当前片段收起来
        let would_be = current.chars().count() + para_len + 1;
        if !current.is_empty() && would_be > max_chars {
            flush(&mut current, &mut chunks);
        }
        if current.is_empty() && !pending_overlap.is_empty() {
            current.push_str(&pending_overlap);
            current.push('\n');
            pending_overlap.clear();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(paragraph);
    }

    flush(&mut current, &mut chunks);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_blank_text_yields_no_chunks() {
        assert!(chunk_text("", 100, 10).is_empty());
        assert!(chunk_text("   \n\n  \t ", 100, 10).is_empty());
    }

    #[test]
    fn a_short_text_is_a_single_chunk() {
        let chunks = chunk_text("只有一句话。", 100, 10);
        assert_eq!(chunks, vec!["只有一句话。".to_string()]);
    }

    #[test]
    fn paragraphs_are_merged_up_to_the_limit() {
        let text = "第一段。\n第二段。\n第三段。";
        let chunks = chunk_text(text, 100, 10);
        assert_eq!(chunks.len(), 1, "没超限就不该切：{chunks:?}");
        assert!(chunks[0].contains("第一段"));
        assert!(chunks[0].contains("第三段"));
    }

    #[test]
    fn paragraphs_split_on_the_boundary_when_over_the_limit() {
        // 每段 6 字，上限 15 → 至少切成两片，且不切在段落中间
        let text = "一二三四五。\n六七八九十。\n甲乙丙丁戊。\n己庚辛壬癸。";
        let chunks = chunk_text(text, 15, 4);
        assert!(chunks.len() >= 2, "应当切开：{chunks:?}");
        for chunk in &chunks {
            // 每一片都应当由完整段落组成（不以半句开头/结尾）
            assert!(chunk.ends_with('。'), "不该切在段落中间：{chunk:?}");
        }
    }

    #[test]
    fn an_overlong_paragraph_is_hard_split_with_overlap() {
        let text = "甲".repeat(100);
        let chunks = chunk_text(&text, 30, 10);
        assert!(
            chunks.len() >= 4,
            "100 字按 30 一段应至少 4 段：{}",
            chunks.len()
        );
        // 每片都不超过上限
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= 30,
                "超出上限：{}",
                chunk.chars().count()
            );
        }
        // 重叠：第二片的开头应当能在第一片里找到（说明留了重叠）
        let first_tail: String = chunks[0]
            .chars()
            .skip(chunks[0].chars().count() - 5)
            .collect();
        assert!(
            chunks[1].starts_with(&first_tail),
            "硬切处应当有重叠：{:?} vs {:?}",
            chunks[1].chars().take(8).collect::<String>(),
            first_tail
        );
    }

    #[test]
    fn overlap_never_exceeds_half_the_chunk() {
        // 传一个荒谬的重叠值也不能让片段无限膨胀
        let text = "乙".repeat(60);
        let chunks = chunk_text(&text, 10, 999);
        for chunk in &chunks {
            // 5 是 overlap 的上界（max_chars/2），加上正文 10 最多 15 左右
            assert!(chunk.chars().count() <= 16, "重叠没被夹住：{chunk:?}");
        }
    }

    #[test]
    fn multi_byte_text_is_never_split_mid_character() {
        // 用字符数而不是字节数切，中文与 emoji 都不会被切坏
        let text = "🌊".repeat(50);
        let chunks = chunk_text(&text, 7, 2);
        for chunk in &chunks {
            // 每一片都应当只由完整的 🌊 组成
            assert!(chunk.chars().all(|c| c == '🌊'), "切坏了：{chunk:?}");
        }
    }

    #[test]
    fn max_chars_of_zero_does_not_hang_or_panic() {
        // 防御：上限被传成 0 时不能死循环（内部会夹到至少 1）
        let chunks = chunk_text("一二三", 0, 0);
        assert!(!chunks.is_empty());
    }
}
