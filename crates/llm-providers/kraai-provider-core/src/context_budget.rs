use kraai_types::ConversationItem;

const COMPACTION_PERCENT: usize = 80;

pub fn compaction_point(max_context: usize) -> usize {
    max_context.saturating_mul(COMPACTION_PERCENT) / 100
}

impl crate::Model {
    /// A response stops at the compaction point to leave room for the compaction request, which
    /// itself may use the rest of the window.
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

    pub fn output_limited_by_context(&self, messages: &[ConversationItem]) -> bool {
        self.output_limit(messages)
            .is_some_and(|limit| self.max_output.is_none_or(|max| limit < max))
    }
}

#[cfg(test)]
mod tests;
