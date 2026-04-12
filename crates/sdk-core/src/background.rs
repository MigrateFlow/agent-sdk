/// A result delivered from a background agent (subagent or team) back to the
/// parent agent's conversation.  The agent loop drains these before each LLM
/// call and injects them as user-role messages so the model can reference them.
#[derive(Debug, Clone)]
pub struct BackgroundResult {
    /// Human-readable name (e.g. "explore", "backend-team").
    pub name: String,
    /// Whether this was a subagent or a team.
    pub kind: BackgroundResultKind,
    /// The final content / summary produced by the background agent.
    pub content: String,
    /// Token usage.
    pub tokens_used: u64,
}

#[derive(Debug, Clone)]
pub enum BackgroundResultKind {
    SubAgent,
    AgentTeam,
    /// Partial/intermediate result from a running subagent.
    SubAgentPartial,
    /// A compaction summary produced by an off-loop summarization subagent.
    /// `target_window_start` / `target_window_end` mark the range of messages
    /// that should be replaced by the summary (half-open interval
    /// `[start, end)`). `window_digest` is a hash of the original window used
    /// to detect if intervening writes have shifted the target range — in
    /// that case the summary is dropped.
    CompactionSummary {
        target_window_start: usize,
        target_window_end: usize,
        window_digest: u64,
        strategy: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_result_is_cloneable_and_debuggable() {
        let r = BackgroundResult {
            name: "n".into(),
            kind: BackgroundResultKind::SubAgent,
            content: "c".into(),
            tokens_used: 1,
        };
        let cloned = r.clone();
        assert_eq!(cloned.name, "n");
        assert!(!format!("{:?}", r).is_empty());
    }

    #[test]
    fn compaction_summary_carries_window_fields() {
        let k = BackgroundResultKind::CompactionSummary {
            target_window_start: 3,
            target_window_end: 7,
            window_digest: 0xdead_beef,
            strategy: "lossy".into(),
        };
        match k {
            BackgroundResultKind::CompactionSummary {
                target_window_start,
                target_window_end,
                window_digest,
                strategy,
            } => {
                assert_eq!(target_window_start, 3);
                assert_eq!(target_window_end, 7);
                assert_eq!(window_digest, 0xdead_beef);
                assert_eq!(strategy, "lossy");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn all_kinds_are_cloneable() {
        for k in [
            BackgroundResultKind::SubAgent,
            BackgroundResultKind::AgentTeam,
            BackgroundResultKind::SubAgentPartial,
        ] {
            let _ = k.clone();
        }
    }
}
