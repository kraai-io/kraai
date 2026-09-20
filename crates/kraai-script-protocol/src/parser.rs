use std::time::Duration;

use kraai_types::SandboxCapabilities;

use crate::ProtocolError;
use crate::payload::parse_script_input;

const OPEN_PREFIX: &str = "<tool_call";
const CLOSE_TAG: &str = "</tool_call>";
const THINK_OPEN_TAG: &str = "<think>";
const THINK_CLOSE_TAG: &str = "</think>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptBlock {
    pub input: String,
    pub source: Vec<u8>,
    pub timeout: Duration,
    pub requested_capabilities: SandboxCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidScriptBlock {
    pub input: String,
    pub source: Vec<u8>,
    pub timeout: Option<Duration>,
    pub requested_capabilities: SandboxCapabilities,
}

#[derive(Debug, Default)]
pub struct IngestResult {
    pub accepted: String,
    pub completed: Option<ScriptBlock>,
    pub error: Option<ProtocolError>,
    pub should_stop: bool,
}

#[derive(Debug, Default)]
pub struct ScriptProtocolParser {
    phase: Phase,
    buffer: String,
    source: String,
    think_depth: usize,
    opening_scan_from: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Preamble,
    Script,
    Finished,
}

impl ScriptProtocolParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, chunk: &str) -> IngestResult {
        if self.phase == Phase::Finished {
            return IngestResult {
                should_stop: true,
                ..IngestResult::default()
            };
        }
        self.buffer.push_str(chunk);
        let mut result = IngestResult::default();
        let mut consumed = 0;
        loop {
            let buffer = &self.buffer[consumed..];
            match self.phase {
                Phase::Preamble => {
                    let Some(start) = buffer.find('<') else {
                        result.accepted.push_str(buffer);
                        consumed = self.buffer.len();
                        break;
                    };
                    result.accepted.push_str(&buffer[..start]);
                    consumed += start;
                    let buffer = &self.buffer[consumed..];
                    if buffer.starts_with(THINK_OPEN_TAG) {
                        result.accepted.push_str(THINK_OPEN_TAG);
                        consumed += THINK_OPEN_TAG.len();
                        self.think_depth = self.think_depth.saturating_add(1);
                    } else if buffer.starts_with(THINK_CLOSE_TAG) {
                        result.accepted.push_str(THINK_CLOSE_TAG);
                        consumed += THINK_CLOSE_TAG.len();
                        self.think_depth = self.think_depth.saturating_sub(1);
                    } else if is_partial_think_tag(buffer) {
                        break;
                    } else if self.think_depth == 0 && is_possible_opening(buffer) {
                        let (_, unscanned) = buffer.split_at(self.opening_scan_from);
                        let Some(end) = unscanned.find('>') else {
                            self.opening_scan_from = buffer.len();
                            break;
                        };
                        let tag_end = self.opening_scan_from + end + 1;
                        self.opening_scan_from = 0;
                        consumed += tag_end;
                        match parse_open_tag(&buffer[..tag_end]) {
                            Ok(()) => {
                                self.phase = Phase::Script;
                            }
                            Err(error) => {
                                self.phase = Phase::Finished;
                                consumed = self.buffer.len();
                                result.error = Some(error);
                                result.should_stop = true;
                                break;
                            }
                        }
                    } else if self.think_depth == 0 && OPEN_PREFIX.starts_with(buffer) {
                        break;
                    } else {
                        self.opening_scan_from = 0;
                        let Some(character) = buffer.chars().next() else {
                            break;
                        };
                        result.accepted.push(character);
                        consumed += character.len_utf8();
                    }
                }
                Phase::Script => {
                    if let Some(close) = buffer.find(CLOSE_TAG) {
                        self.source.push_str(buffer.split_at(close).0);
                        consumed = self.buffer.len();
                        self.phase = Phase::Finished;
                        result.should_stop = true;
                        match parse_script_input(&self.source) {
                            Ok(script) => result.completed = Some(script),
                            Err(error) => result.error = Some(error),
                        }
                        break;
                    }
                    let keep = partial_suffix_len(buffer, CLOSE_TAG);
                    let safe = buffer.len().saturating_sub(keep);
                    if safe == 0 {
                        break;
                    }
                    self.source.push_str(buffer.split_at(safe).0);
                    consumed += safe;
                }
                Phase::Finished => {
                    consumed = self.buffer.len();
                    result.should_stop = true;
                    break;
                }
            }
        }
        self.buffer.drain(..consumed);
        result
    }

    pub fn finish(&mut self) -> IngestResult {
        self.opening_scan_from = 0;
        match self.phase {
            Phase::Preamble => IngestResult {
                accepted: std::mem::take(&mut self.buffer),
                ..IngestResult::default()
            },
            Phase::Script => {
                self.source.push_str(&self.buffer);
                self.buffer.clear();
                self.phase = Phase::Finished;
                IngestResult {
                    error: Some(ProtocolError::IncompleteScript),
                    should_stop: true,
                    ..IngestResult::default()
                }
            }
            Phase::Finished => IngestResult {
                should_stop: true,
                ..IngestResult::default()
            },
        }
    }

    pub fn invalid_block(&self) -> InvalidScriptBlock {
        InvalidScriptBlock {
            input: self.source.clone(),
            source: self.source.as_bytes().to_vec(),
            timeout: None,
            requested_capabilities: SandboxCapabilities::default(),
        }
    }
}

fn parse_open_tag(tag: &str) -> Result<(), ProtocolError> {
    if tag == "<tool_call>" {
        Ok(())
    } else {
        Err(ProtocolError::MalformedStartTag(String::from(
            "attributes are not allowed; put timeout and permissions in the script metadata comment",
        )))
    }
}

fn is_possible_opening(input: &str) -> bool {
    let Some(tail) = input.strip_prefix(OPEN_PREFIX) else {
        return false;
    };
    tail.is_empty()
        || tail.starts_with('>')
        || tail.starts_with(|character: char| character.is_ascii_whitespace())
}

fn is_partial_think_tag(input: &str) -> bool {
    THINK_OPEN_TAG.starts_with(input) || THINK_CLOSE_TAG.starts_with(input)
}

fn partial_suffix_len(input: &str, pattern: &str) -> usize {
    let max = input.len().min(pattern.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|length| input.ends_with(&pattern[..*length]))
        .unwrap_or(0)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "parser tests directly assert completed protocol values"
)]
mod tests {
    use super::ScriptProtocolParser;
    use crate::ProtocolError;
    use kraai_types::SandboxCapability;
    use std::time::Duration;

    #[test]
    fn streams_preamble_and_discards_same_chunk_trailing_output() {
        let mut parser = ScriptProtocolParser::new();
        let result = parser.ingest(
            "I will inspect it.\n<tool_call>\n# timeout=30sec\nls | where size > 0\n</tool_call>\nwaiting",
        );
        assert!(result.should_stop);
        assert_eq!(result.accepted, "I will inspect it.\n");
        let completed = result.completed.expect("completed script");
        assert_eq!(completed.source, b"ls | where size > 0\n");
        assert_eq!(completed.timeout, Duration::from_secs(30));
    }

    #[test]
    fn delimiter_and_attribute_splits_are_equivalent_at_every_boundary() {
        let input = "Préamble 🦀\n<tool_call>\n# timeout=1.5sec permissions=workspace-write,network\n[1 2] | math sum\n</tool_call>ignored";
        let boundaries = input
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(input.len()))
            .collect::<Vec<_>>();
        for boundary in boundaries {
            let mut parser = ScriptProtocolParser::new();
            let first = parser.ingest(&input[..boundary]);
            let second = parser.ingest(&input[boundary..]);
            let accepted = format!("{}{}", first.accepted, second.accepted);
            assert_eq!(accepted, "Préamble 🦀\n", "boundary {boundary}");
            let completed = first
                .completed
                .or(second.completed)
                .expect("completed script");
            assert_eq!(completed.source, b"[1 2] | math sum\n");
            assert_eq!(completed.timeout, Duration::from_millis(1500));
            assert!(
                completed
                    .requested_capabilities
                    .contains(SandboxCapability::WorkspaceWrite)
            );
            assert!(
                completed
                    .requested_capabilities
                    .contains(SandboxCapability::Network)
            );
        }
    }

    #[test]
    fn malformed_and_incomplete_scripts_fail_closed() {
        let mut parser = ScriptProtocolParser::new();
        let result = parser.ingest("<tool_call>\n# permissions=network\necho hi\n</tool_call>");
        assert_eq!(result.error, Some(ProtocolError::MissingTimeout));
        assert!(result.should_stop);

        let mut parser = ScriptProtocolParser::new();
        let first = parser.ingest("<tool_call>\n# timeout=1sec\necho hi");
        assert!(first.error.is_none());
        let end = parser.finish();
        assert_eq!(end.error, Some(ProtocolError::IncompleteScript));
    }

    #[test]
    fn ordinary_less_than_text_is_not_mistaken_for_a_script() {
        let mut parser = ScriptProtocolParser::new();
        let first = parser.ingest("Use <tool_callback> and 1 < 2");
        let tail = parser.finish();
        assert_eq!(
            format!("{}{}", first.accepted, tail.accepted),
            "Use <tool_callback> and 1 < 2"
        );
        assert!(first.completed.is_none());
    }

    #[test]
    fn partial_opening_can_become_literal_before_a_later_script() {
        let mut parser = ScriptProtocolParser::new();
        assert!(parser.ingest("<tool_call").accepted.is_empty());
        let result = parser.ingest("back> <tool_call>\n# timeout=1sec\necho ok\n</tool_call>");
        assert_eq!(result.accepted, "<tool_callback> ");
        assert_eq!(result.completed.expect("later script").source, b"echo ok\n");
    }

    #[test]
    fn tool_calls_inside_think_blocks_are_inert_across_every_split() {
        let input = "<think>\n<tool_call timeout=\"1sec\">bad\n</tool_call>\n</think>\n<tool_call>\n# timeout=2sec\ngood\n</tool_call>ignored";
        for split in input
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(input.len()))
        {
            let mut parser = ScriptProtocolParser::new();
            let first = parser.ingest(&input[..split]);
            let second = parser.ingest(&input[split..]);
            let completed = first.completed.or(second.completed).expect("script");
            assert_eq!(completed.source, b"good\n");
        }
    }

    #[test]
    fn nested_think_and_script_fragments_preserve_each_ingest_result() {
        let mut parser = ScriptProtocolParser::new();
        for (chunk, accepted) in [
            ("<thi", ""),
            ("nk>α<thi", "<think>α"),
            (
                "nk><tool_call>bogus</tool_call></thi",
                "<think><tool_call>bogus</tool_call>",
            ),
            ("nk>β</think>γ<tool_", "</think>β</think>γ"),
            ("call>\n# timeout=1sec\nécho\n</tool", ""),
        ] {
            let result = parser.ingest(chunk);
            assert_eq!(result.accepted, accepted);
            assert!(result.completed.is_none());
            assert!(result.error.is_none());
            assert!(!result.should_stop);
        }
        let result = parser.ingest("_call>tail<tool_call>ignored</tool_call>");
        assert!(result.accepted.is_empty());
        assert!(result.error.is_none());
        assert!(result.should_stop);
        assert_eq!(
            result.completed.expect("script").source,
            "écho\n".as_bytes()
        );
        let result = parser.ingest("discarded");
        assert!(result.accepted.is_empty());
        assert!(result.completed.is_none());
        assert!(result.error.is_none());
        assert!(result.should_stop);
        let result = parser.finish();
        assert!(result.accepted.is_empty());
        assert!(result.error.is_none());
        assert!(result.should_stop);
    }

    #[test]
    fn incomplete_prefixes_and_invalid_tags_preserve_buffered_text() {
        for prefix in ["<", "<thi", "</thin", "<tool_cal", "<tool_call "] {
            let mut parser = ScriptProtocolParser::new();
            let result = parser.ingest(&format!("🦀 {prefix}"));
            assert_eq!(result.accepted, "🦀 ");
            assert!(result.error.is_none());
            assert!(!result.should_stop);
            let result = parser.finish();
            assert_eq!(result.accepted, prefix);
            assert!(result.error.is_none());
            assert!(!result.should_stop);
        }
        let mut parser = ScriptProtocolParser::new();
        let first = parser.ingest("α<tool_call time");
        assert_eq!(first.accepted, "α");
        assert!(!first.should_stop);
        let second = parser.ingest("out='1sec'>ignored");
        assert!(second.accepted.is_empty());
        assert!(matches!(
            second.error,
            Some(ProtocolError::MalformedStartTag(_))
        ));
        assert!(second.should_stop);
        assert!(parser.invalid_block().source.is_empty());
        assert!(parser.finish().should_stop);
    }

    #[test]
    fn large_literal_tag_preamble_preserves_text_and_nested_depth() {
        let text = "<α<tool_callback><think><think>β</think></think>".repeat(4096);
        let mut parser = ScriptProtocolParser::new();
        let result = parser.ingest(&text);
        assert_eq!(result.accepted, text);
        assert!(result.completed.is_none());
        assert!(result.error.is_none());
        assert!(!result.should_stop);
        let result = parser.ingest("<tool_call>\n# timeout=1sec\necho ok\n</tool_call>");
        assert_eq!(result.completed.expect("script").source, b"echo ok\n");
        assert!(result.should_stop);
    }

    #[test]
    fn fragmented_long_opening_waits_for_terminator_and_finish_allows_reuse() {
        let mut parser = ScriptProtocolParser::new();
        assert_eq!(parser.ingest("prefix <tool_call ").accepted, "prefix ");
        let fragment = "attribute=α🦀 ".repeat(32);
        for _ in 0..512 {
            let result = parser.ingest(&fragment);
            assert!(result.accepted.is_empty());
            assert!(result.completed.is_none());
            assert!(result.error.is_none());
            assert!(!result.should_stop);
        }
        let result = parser.ingest(">");
        assert!(matches!(
            result.error,
            Some(ProtocolError::MalformedStartTag(_))
        ));
        assert!(result.should_stop);
        assert!(parser.invalid_block().source.is_empty());

        let mut parser = ScriptProtocolParser::new();
        parser.ingest("<tool_call ");
        parser.ingest(&fragment);
        let result = parser.finish();
        assert_eq!(result.accepted, format!("<tool_call {fragment}"));
        assert!(!result.should_stop);
        let result = parser.ingest("<tool_call>\n# timeout=1sec\nécho 🦀\n</tool_call>");
        assert_eq!(
            result.completed.expect("script after finish").source,
            "écho 🦀\n".as_bytes()
        );
    }

    #[test]
    fn incomplete_unicode_source_preserves_invalid_block_bytes() {
        let mut parser = ScriptProtocolParser::new();
        let source = "\r\n# timeout=1sec\r\n'é 🦀'\n";
        parser.ingest("<tool_call>");
        for character in source.chars() {
            let result = parser.ingest(character.encode_utf8(&mut [0; 4]));
            assert!(result.completed.is_none());
            assert!(result.error.is_none());
        }
        parser.ingest("</tool_");
        assert_eq!(parser.invalid_block().input, source);
        assert_eq!(parser.invalid_block().source, source.as_bytes());
        assert_eq!(parser.finish().error, Some(ProtocolError::IncompleteScript));
        let invalid = parser.invalid_block();
        let expected = format!("{source}</tool_");
        assert_eq!(invalid.input, expected);
        assert_eq!(invalid.source, expected.as_bytes());
    }
}
