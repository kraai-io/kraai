use kraai_types::ConversationItem;

const COMPACTION_PERCENT: usize = 80;

pub fn compaction_point(max_context: usize) -> usize {
    max_context.saturating_mul(COMPACTION_PERCENT) / 100
}

pub fn max_output(
    configured: Option<usize>,
    catalog: Option<usize>,
    max_context: Option<usize>,
) -> Option<usize> {
    configured.or_else(|| {
        let reserve = max_context.map(|limit| limit - compaction_point(limit));
        catalog.into_iter().chain(reserve).min()
    })
}

impl crate::Model {
    pub fn output_limit(&self, messages: &[ConversationItem]) -> Option<usize> {
        let remaining = self.max_context.map(|limit| {
            limit.saturating_sub(
                messages
                    .iter()
                    .map(ConversationItem::estimated_tokens)
                    .sum(),
            )
        });
        self.max_output
            .into_iter()
            .chain(remaining)
            .min()
            .filter(|tokens| *tokens > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(max_context: Option<usize>, max_output: Option<usize>) -> crate::Model {
        crate::Model {
            id: kraai_types::ModelId::new("model"),
            name: String::from("model"),
            max_context,
            max_output,
            supports_images: false,
            options: Vec::new(),
        }
    }

    #[test]
    fn the_default_output_limit_keeps_input_and_output_inside_the_window() {
        assert_eq!(compaction_point(262_144), 209_715);
        assert_eq!(max_output(None, Some(235_929), Some(262_144)), Some(52_429));
        assert_eq!(max_output(None, Some(32_000), Some(262_144)), Some(32_000));
        assert_eq!(max_output(None, None, Some(16_384)), Some(3_277));
        assert_eq!(max_output(None, Some(8_192), None), Some(8_192));
        assert_eq!(max_output(None, None, None), None);
    }

    #[test]
    fn a_configured_output_limit_replaces_the_default() {
        assert_eq!(max_output(Some(8_192), None, Some(16_384)), Some(8_192));
        assert_eq!(
            max_output(Some(100), Some(32_000), Some(262_144)),
            Some(100)
        );
    }

    #[test]
    fn a_response_never_requests_more_than_the_window_left_after_its_input() {
        let input = vec![ConversationItem::User {
            content: "a".repeat(4_000 * kraai_types::BYTES_PER_TOKEN).into(),
        }];
        let input_tokens: usize = input.iter().map(ConversationItem::estimated_tokens).sum();
        assert_eq!(
            model(Some(16_384), Some(8_192)).output_limit(&input),
            Some(8_192)
        );
        assert_eq!(
            model(Some(10_000), Some(8_192)).output_limit(&input),
            Some(10_000 - input_tokens)
        );
        assert_eq!(model(Some(1_000), Some(8_192)).output_limit(&input), None);
        assert_eq!(model(None, Some(8_192)).output_limit(&input), Some(8_192));
        assert_eq!(model(None, None).output_limit(&input), None);
    }
}
