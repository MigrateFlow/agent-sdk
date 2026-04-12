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
