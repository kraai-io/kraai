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
    source: Vec<u8>,
    think_depth: usize,
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
        loop {
            match self.phase {
                Phase::Preamble => {
                    let Some(start) = self.buffer.find('<') else {
                        result.accepted.push_str(&self.buffer);
                        self.buffer.clear();
                        break;
                    };
                    result.accepted.push_str(&self.buffer[..start]);
                    self.buffer.drain(..start);
                    if self.buffer.starts_with(THINK_OPEN_TAG) {
                        result.accepted.push_str(THINK_OPEN_TAG);
                        self.buffer.drain(..THINK_OPEN_TAG.len());
                        self.think_depth = self.think_depth.saturating_add(1);
                    } else if self.buffer.starts_with(THINK_CLOSE_TAG) {
                        result.accepted.push_str(THINK_CLOSE_TAG);
                        self.buffer.drain(..THINK_CLOSE_TAG.len());
                        self.think_depth = self.think_depth.saturating_sub(1);
                    } else if is_partial_think_tag(&self.buffer) {
                        break;
                    } else if self.think_depth == 0 && is_possible_opening(&self.buffer) {
                        let Some(end) = self.buffer.find('>') else {
                            break;
                        };
                        let tag_end = end + 1;
                        let tag = self.buffer[..tag_end].to_owned();
                        self.buffer.drain(..tag_end);
                        match parse_open_tag(&tag) {
                            Ok(()) => {
                                self.phase = Phase::Script;
                            }
                            Err(error) => {
                                self.phase = Phase::Finished;
                                self.buffer.clear();
                                result.error = Some(error);
                                result.should_stop = true;
                                break;
                            }
                        }
                    } else if self.think_depth == 0 && OPEN_PREFIX.starts_with(&self.buffer) {
                        break;
                    } else {
                        let Some(character) = self.buffer.chars().next() else {
                            break;
                        };
                        result.accepted.push(character);
                        self.buffer.drain(..character.len_utf8());
                    }
                }
                Phase::Script => {
                    if let Some(close) = self.buffer.find(CLOSE_TAG) {
                        self.source.extend(self.buffer.bytes().take(close));
                        self.buffer.clear();
                        self.phase = Phase::Finished;
                        result.should_stop = true;
                        let input = String::from_utf8_lossy(&self.source).into_owned();
                        match parse_script_input(&input) {
                            Ok(script) => result.completed = Some(script),
                            Err(error) => result.error = Some(error),
                        }
                        break;
                    }
                    let keep = partial_suffix_len(&self.buffer, CLOSE_TAG);
                    let safe = self.buffer.len().saturating_sub(keep);
                    if safe == 0 {
                        break;
                    }
                    self.source.extend(self.buffer.bytes().take(safe));
                    self.buffer.drain(..safe);
                }
                Phase::Finished => {
                    self.buffer.clear();
                    result.should_stop = true;
                    break;
                }
            }
        }
        result
    }

    pub fn finish(&mut self) -> IngestResult {
        match self.phase {
            Phase::Preamble => IngestResult {
                accepted: std::mem::take(&mut self.buffer),
                ..IngestResult::default()
            },
            Phase::Script => {
                self.source.extend_from_slice(self.buffer.as_bytes());
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
        let input = String::from_utf8_lossy(&self.source).into_owned();
        InvalidScriptBlock {
            input,
            source: self.source.clone(),
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
}
