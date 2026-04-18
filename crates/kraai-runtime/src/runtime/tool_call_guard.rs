const TOOL_CALL_OPEN_TAG: &str = "<tool_call>";
const TOOL_CALL_CLOSE_TAG: &str = "</tool_call>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ToolCallStreamPhase {
    #[default]
    PrefixVisible,
    PrefixInsideThink,
    InsideToolCall,
    AfterToolCall,
}

#[derive(Debug, Default)]
pub(crate) struct ToolCallStreamGuard {
    phase: ToolCallStreamPhase,
    buffer: String,
}

#[derive(Debug, Default)]
pub(crate) struct ToolCallGuardChunkResult {
    pub(crate) accepted: String,
    pub(crate) should_stop: bool,
}

impl ToolCallStreamGuard {
    pub(crate) fn ingest_chunk(&mut self, chunk: &str) -> ToolCallGuardChunkResult {
        self.buffer.push_str(chunk);

        let mut accepted = String::new();
        let mut cursor = 0usize;
        let mut should_stop = false;

        while cursor < self.buffer.len() {
            let remaining = &self.buffer[cursor..];

            match self.phase {
                ToolCallStreamPhase::PrefixVisible => {
                    if remaining.starts_with(TOOL_CALL_OPEN_TAG) {
                        accepted.push_str(TOOL_CALL_OPEN_TAG);
                        cursor += TOOL_CALL_OPEN_TAG.len();
                        self.phase = ToolCallStreamPhase::InsideToolCall;
                        continue;
                    }

                    if is_partial_prefix(remaining, TOOL_CALL_OPEN_TAG)
                        || is_possible_open_think_tag_prefix(remaining)
                    {
                        break;
                    }

                    if let Some(tag) = parse_full_think_tag_at_start(remaining) {
                        accepted.push_str(&remaining[..tag.len]);
                        cursor += tag.len;
                        if !tag.closing {
                            self.phase = ToolCallStreamPhase::PrefixInsideThink;
                        }
                        continue;
                    }

                    let ch = remaining
                        .chars()
                        .next()
                        .expect("remaining content should have a character");
                    accepted.push(ch);
                    cursor += ch.len_utf8();
                }
                ToolCallStreamPhase::PrefixInsideThink => {
                    if let Some(tag) = parse_full_think_tag_at_start(remaining) {
                        accepted.push_str(&remaining[..tag.len]);
                        cursor += tag.len;
                        if tag.closing {
                            self.phase = ToolCallStreamPhase::PrefixVisible;
                        }
                        continue;
                    }

                    if is_possible_close_think_tag_prefix(remaining) {
                        break;
                    }

                    let ch = remaining
                        .chars()
                        .next()
                        .expect("remaining content should have a character");
                    accepted.push(ch);
                    cursor += ch.len_utf8();
                }
                ToolCallStreamPhase::InsideToolCall => {
                    if let Some(close_index) = remaining.find(TOOL_CALL_CLOSE_TAG) {
                        let close_end = close_index + TOOL_CALL_CLOSE_TAG.len();
                        accepted.push_str(&remaining[..close_end]);
                        cursor += close_end;
                        self.phase = ToolCallStreamPhase::AfterToolCall;
                        continue;
                    }

                    let keep_len = partial_suffix_len(remaining, TOOL_CALL_CLOSE_TAG);
                    let safe_len = remaining.len().saturating_sub(keep_len);
                    if safe_len == 0 {
                        break;
                    }

                    accepted.push_str(&remaining[..safe_len]);
                    cursor += safe_len;
                }
                ToolCallStreamPhase::AfterToolCall => {
                    let whitespace_len = remaining
                        .chars()
                        .take_while(|ch| ch.is_whitespace())
                        .map(char::len_utf8)
                        .sum();
                    if whitespace_len > 0 {
                        accepted.push_str(&remaining[..whitespace_len]);
                        cursor += whitespace_len;
                        continue;
                    }

                    if remaining.starts_with(TOOL_CALL_OPEN_TAG) {
                        accepted.push_str(TOOL_CALL_OPEN_TAG);
                        cursor += TOOL_CALL_OPEN_TAG.len();
                        self.phase = ToolCallStreamPhase::InsideToolCall;
                        continue;
                    }

                    if is_partial_prefix(remaining, TOOL_CALL_OPEN_TAG) {
                        break;
                    }

                    should_stop = true;
                    break;
                }
            }
        }

        if should_stop {
            self.buffer.clear();
        } else {
            self.buffer.drain(..cursor);
        }

        ToolCallGuardChunkResult {
            accepted,
            should_stop,
        }
    }

    pub(crate) fn finish(&mut self) -> String {
        match self.phase {
            ToolCallStreamPhase::AfterToolCall => {
                self.buffer.clear();
                String::new()
            }
            ToolCallStreamPhase::PrefixVisible
            | ToolCallStreamPhase::PrefixInsideThink
            | ToolCallStreamPhase::InsideToolCall => std::mem::take(&mut self.buffer),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedThinkTag {
    len: usize,
    closing: bool,
}

fn is_partial_prefix(input: &str, pattern: &str) -> bool {
    !input.is_empty() && input.len() < pattern.len() && pattern.starts_with(input)
}

fn partial_suffix_len(input: &str, pattern: &str) -> usize {
    let max_len = input.len().min(pattern.len().saturating_sub(1));
    for len in (1..=max_len).rev() {
        if input.ends_with(&pattern[..len]) {
            return len;
        }
    }
    0
}

fn parse_full_think_tag_at_start(input: &str) -> Option<ParsedThinkTag> {
    if !input.starts_with('<') {
        return None;
    }

    let bytes = input.as_bytes();
    let mut cursor = 1usize;
    let closing = matches!(bytes.get(cursor), Some(b'/'));
    if closing {
        cursor += 1;
    }

    let name_len = if input[cursor..].len() >= "thinking".len()
        && input[cursor..cursor + "thinking".len()].eq_ignore_ascii_case("thinking")
    {
        "thinking".len()
    } else if input[cursor..].len() >= "think".len()
        && input[cursor..cursor + "think".len()].eq_ignore_ascii_case("think")
    {
        "think".len()
    } else {
        return None;
    };
    cursor += name_len;

    let next = input[cursor..].chars().next()?;
    if next.is_ascii_alphanumeric() || next == '_' {
        return None;
    }

    let close_len = input[cursor..].find('>')?;
    Some(ParsedThinkTag {
        len: cursor + close_len + 1,
        closing,
    })
}

fn is_possible_open_think_tag_prefix(input: &str) -> bool {
    is_possible_think_tag_prefix(input, false)
}

fn is_possible_close_think_tag_prefix(input: &str) -> bool {
    is_possible_think_tag_prefix(input, true)
}

fn is_possible_think_tag_prefix(input: &str, closing: bool) -> bool {
    if !input.starts_with('<') || input.contains('>') {
        return false;
    }

    let bytes = input.as_bytes();
    let mut cursor = 1usize;

    if closing {
        if bytes.get(cursor) != Some(&b'/') {
            return false;
        }
        cursor += 1;
    } else if bytes.get(cursor) == Some(&b'/') {
        return false;
    }

    let name = &input[cursor..];
    if name.is_empty() {
        return true;
    }

    let letters_len = name
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .count();
    let letters = &name[..letters_len];

    let matches_prefix = ["think", "thinking"]
        .iter()
        .any(|candidate| candidate.starts_with(&letters.to_ascii_lowercase()));
    if !matches_prefix {
        return false;
    }

    if letters_len == name.len() {
        return true;
    }

    let remainder = &name[letters_len..];
    let next = remainder
        .chars()
        .next()
        .expect("remainder should contain a character");
    let matched_full_name = ["think", "thinking"]
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(letters));

    matched_full_name && !(next.is_ascii_alphanumeric() || next == '_')
}

#[cfg(test)]
mod tests {
    use super::ToolCallStreamGuard;

    #[test]
    fn tool_call_stream_guard_stops_on_trailing_non_whitespace() {
        let mut guard = ToolCallStreamGuard::default();

        let result = guard.ingest_chunk(
            "before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\nHallucinated",
        );

        assert!(result.should_stop);
        assert_eq!(
            result.accepted,
            "before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n"
        );
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn tool_call_stream_guard_allows_adjacent_tool_calls_across_chunks() {
        let mut guard = ToolCallStreamGuard::default();

        let first = guard
            .ingest_chunk("before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n<to");
        assert!(!first.should_stop);
        assert_eq!(
            first.accepted,
            "before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n"
        );

        let second = guard.ingest_chunk("ol_call>\ntool: auto_tool\nvalue: beta\n</tool_call>\n");
        assert!(!second.should_stop);
        assert_eq!(
            second.accepted,
            "<tool_call>\ntool: auto_tool\nvalue: beta\n</tool_call>\n"
        );
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn tool_call_stream_guard_ignores_hidden_tool_calls_inside_prefix_think_blocks() {
        let mut guard = ToolCallStreamGuard::default();

        let result = guard.ingest_chunk(
            "<thinking class=\"chain\">\n\
<tool_call>\n\
tool: hidden_tool\n\
</tool_call>\n\
</thinking>\n\
before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n\
after",
        );

        assert!(result.should_stop);
        assert_eq!(
            result.accepted,
            "<thinking class=\"chain\">\n\
<tool_call>\n\
tool: hidden_tool\n\
</tool_call>\n\
</thinking>\n\
before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n"
        );
    }

    #[test]
    fn tool_call_stream_guard_drops_incomplete_next_tool_call_at_finish() {
        let mut guard = ToolCallStreamGuard::default();

        let result =
            guard.ingest_chunk("<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n<too");

        assert!(!result.should_stop);
        assert_eq!(
            result.accepted,
            "<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n"
        );
        assert!(guard.finish().is_empty());
    }
}
