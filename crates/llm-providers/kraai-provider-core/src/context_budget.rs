use kraai_types::ConversationItem;

const COMPACTION_PERCENT: usize = 80;

pub fn compaction_point(max_context: usize) -> usize {
    max_context.saturating_mul(COMPACTION_PERCENT) / 100
}

impl crate::Model {
    /// The most tokens a response to `messages` may use. A response may fill the context up to
    /// the compaction point, which leaves room for the compaction request that follows it.
    /// Input already past that point, such as the compaction request itself, may use the rest of
    /// the window.
    pub fn output_limit(&self, messages: &[ConversationItem]) -> Option<usize> {
        let input: usize = messages
            .iter()
            .map(ConversationItem::estimated_tokens)
            .sum();
        let room = self.max_context.map(|limit| {
            let point = compaction_point(limit);
            if input < point {
                point - input
            } else {
                limit.saturating_sub(input)
            }
        });
        self.max_output
            .into_iter()
            .chain(room)
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

    fn input(tokens: usize) -> Vec<ConversationItem> {
        let item = |text: String| ConversationItem::User {
            content: text.into(),
        };
        let overhead = item(String::new()).estimated_tokens();
        vec![item(
            "a".repeat((tokens - overhead) * kraai_types::BYTES_PER_TOKEN),
        )]
    }

    #[test]
    fn responses_fill_the_context_up_to_the_compaction_point() {
        let messages = input(5_000);
        assert_eq!(compaction_point(16_000), 12_800);
        assert_eq!(
            model(Some(16_000), None).output_limit(&messages),
            Some(7_800)
        );
        assert_eq!(
            model(Some(16_000), Some(4_096)).output_limit(&messages),
            Some(4_096)
        );
        assert_eq!(
            model(None, Some(4_096)).output_limit(&messages),
            Some(4_096)
        );
        assert_eq!(model(None, None).output_limit(&messages), None);
    }

    #[test]
    fn input_past_the_compaction_point_may_use_the_rest_of_the_window() {
        assert_eq!(
            model(Some(16_000), None).output_limit(&input(13_000)),
            Some(3_000)
        );
        assert_eq!(
            model(Some(16_000), None).output_limit(&input(12_800)),
            Some(3_200)
        );
        assert_eq!(
            model(Some(16_000), Some(8_192)).output_limit(&input(16_000)),
            None
        );
    }
}
