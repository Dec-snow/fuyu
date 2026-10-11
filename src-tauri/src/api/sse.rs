//! 增量式 Server-Sent Events 解析器。
//!
//! 只懂 SSE 的**字节协议**，不懂任何厂商语义（连 `[DONE]` 都不认识）——
//! 语义解析在 [`super::openai::parse_stream_chunk`]。两层解耦之后，两层都
//! 是可以纯函数单测的，不需要网络。
//!
//! 覆盖的场景：
//! - 一个网络分片里包含多个 SSE 事件；
//! - 一个 SSE 事件被拆到多个网络分片；
//! - 空行与 `:` 注释/心跳行；
//! - `\n` 与 `\r\n` 两种行尾；
//! - `[DONE]` 只是普通 payload，交给上层判断；
//! - 任何解析失败都不 panic。

/// 单行上限。服务端若一直不发换行，`buf` 会无限增长。
const MAX_LINE_BYTES: usize = 1024 * 1024; // 1 MiB
/// 单个事件的 `data:` 累计上限。
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

#[derive(Default)]
pub struct SseParser {
    /// 已收到但还没有遇到换行的字节。
    buf: Vec<u8>,
    /// 正在拼装的这个事件已经收到的 `data:` 行。
    data_lines: Vec<String>,
    /// `data_lines` 当前占用的字节数。
    data_bytes: usize,
    /// 任一上限被突破后置位，之后拒绝一切输入。
    overflowed: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// 喂一个网络分片，返回它**凑齐**的所有事件 payload。
    ///
    /// 直接按裸字节 `0x0A` 切行是 UTF-8 安全的：UTF-8 的续字节范围是
    /// `0x80..=0xBF`，永远不等于 `\n`，所以行边界不可能落在字符中间。
    /// 被切断的半截字符会留在 `buf` 里等下一个分片。
    ///
    /// 最容易写错的地方：**只对完整行做 `from_utf8_lossy`**。如果先对整个
    /// `buf` 解码再切行，半截的中文字符就会被替换成 `�`，这就是 LLM 客户端
    /// 中文乱码的头号成因。
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        if self.overflowed {
            return Vec::new();
        }
        self.buf.extend_from_slice(chunk);

        if self.buf.len() > MAX_LINE_BYTES && !self.buf.contains(&b'\n') {
            // 已经超限且还没出现换行 —— 这一行永远不会合法结束。
            self.mark_overflowed();
            return Vec::new();
        }

        let mut events = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // 去掉 '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // 容忍 CRLF
            }
            if line.len() > MAX_LINE_BYTES {
                self.mark_overflowed();
                break;
            }
            self.process_line(&line, &mut events);
            if self.overflowed {
                break;
            }
        }
        events
    }

    /// 流结束时冲刷最后一个未闭合的事件。
    ///
    /// 有些服务商发完最后一段数据就直接关连接，不会补那个空行；
    /// 少了这一步就会丢掉回答的最后一句。
    pub fn finish(&mut self) -> Vec<String> {
        if self.overflowed {
            return Vec::new();
        }
        let mut events = Vec::new();

        if !self.buf.is_empty() {
            let mut line = std::mem::take(&mut self.buf);
            if line.len() > MAX_LINE_BYTES {
                self.mark_overflowed();
                return Vec::new();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut events);
        }
        if !self.data_lines.is_empty() {
            events.push(self.data_lines.join("\n"));
            self.data_lines.clear();
            self.data_bytes = 0;
        }
        events
    }

    fn mark_overflowed(&mut self) {
        self.overflowed = true;
        self.buf.clear();
        self.data_lines.clear();
        self.data_bytes = 0;
    }

    fn process_line(&mut self, line: &[u8], events: &mut Vec<String>) {
        if line.is_empty() {
            // 空行 = 事件边界，派发已攒下的 data。
            if !self.data_lines.is_empty() {
                events.push(self.data_lines.join("\n"));
            }
            self.data_lines.clear();
            self.data_bytes = 0;
            return;
        }
        // `:` 开头是注释/心跳，忽略。
        if line[0] == b':' {
            return;
        }

        // 只有这里做解码，而且只对完整的一行。
        let text = String::from_utf8_lossy(line);
        let (field, value) = match text.split_once(':') {
            // `data: x` 之后**只去掉一个空格**。用 trim_start 会把用户正文里
            // 有意义的缩进吃掉（代码块首行缩进就是这么丢的）。
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (text.as_ref(), ""),
        };

        if field == "data" {
            if self.data_bytes + value.len() > MAX_EVENT_BYTES {
                self.mark_overflowed();
                return;
            }
            self.data_bytes += value.len();
            self.data_lines.push(value.to_string());
        }
        // `event:` / `id:` / `retry:` 当前不需要，显式忽略。
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_events_in_one_chunk() {
        let mut p = SseParser::new();
        assert_eq!(
            p.push(b"data: alpha\n\ndata: beta\n\n"),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: hel").is_empty());
        assert!(p.push(b"lo wor").is_empty());
        assert_eq!(p.push(b"ld\n\n"), vec!["hello world"]);
    }

    #[test]
    fn crlf_is_tolerated() {
        let mut p = SseParser::new();
        assert_eq!(p.push(b"data: crlf\r\n\r\n"), vec!["crlf"]);
    }

    #[test]
    fn comments_and_heartbeats_are_ignored() {
        let mut p = SseParser::new();
        assert_eq!(
            p.push(b": keep-alive\n\n:ping\n\n\n\ndata: real\n\n"),
            vec!["real"]
        );
    }

    #[test]
    fn multiple_data_lines_join_with_newline() {
        let mut p = SseParser::new();
        assert_eq!(
            p.push(b"data: line1\ndata: line2\n\n"),
            vec!["line1\nline2"]
        );
    }

    #[test]
    fn only_one_leading_space_is_stripped() {
        let mut p = SseParser::new();
        // 正文本身以空格开头时，不能把它的缩进也吃掉。
        assert_eq!(p.push(b"data:   indented\n\n"), vec!["  indented"]);
    }

    #[test]
    fn blank_lines_produce_nothing() {
        let mut p = SseParser::new();
        assert!(p.push(b"\n\n\n").is_empty());
        assert!(p.finish().is_empty());
    }

    #[test]
    fn a_runaway_line_is_rejected_not_buffered() {
        let mut p = SseParser::new();
        let chunk = vec![b'a'; MAX_LINE_BYTES + 1];
        assert!(p.push(&chunk).is_empty());
        assert!(p.overflowed());
        assert!(p.push(b"data: late\n\n").is_empty());
        assert!(p.finish().is_empty());
    }

    #[test]
    fn an_oversized_event_is_rejected() {
        let mut p = SseParser::new();
        let line = format!("data: {}\n", "x".repeat(MAX_EVENT_BYTES / 2));
        assert!(p.push(line.as_bytes()).is_empty());
        assert!(p.push(line.as_bytes()).is_empty());
        assert!(p.overflowed());
    }

    #[test]
    fn finish_flushes_an_unterminated_event() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: tail").is_empty());
        assert_eq!(p.finish(), vec!["tail"]);
    }

    #[test]
    fn done_sentinel_is_passed_through_untouched() {
        let mut p = SseParser::new();
        assert_eq!(p.push(b"data: [DONE]\n\n"), vec!["[DONE]"]);
    }

    #[test]
    fn utf8_split_mid_character_is_safe() {
        let mut p = SseParser::new();
        // "你" = E4 BD A0，把字节切在两个分片里。
        assert!(p
            .push(&[b'd', b'a', b't', b'a', b':', b' ', 0xE4, 0xBD])
            .is_empty());
        assert_eq!(p.push(&[0xA0, b'\n', b'\n']), vec!["你"]);
    }

    #[test]
    fn every_multibyte_char_split_at_every_offset() {
        // 把一个由多字节字符组成的 JSON 在每一个可能的字节位置切开，
        // 拼起来必须和一次性喂进去完全一致。
        let payload = "data: {\"t\":\"你好世界🌍\"}\n\n".as_bytes().to_vec();
        let expected = vec!["{\"t\":\"你好世界🌍\"}".to_string()];
        for split in 0..payload.len() {
            let mut p = SseParser::new();
            let mut got = p.push(&payload[..split]);
            got.extend(p.push(&payload[split..]));
            got.extend(p.finish());
            assert_eq!(got, expected, "在字节 {split} 处切开时结果不一致");
        }
    }
}
