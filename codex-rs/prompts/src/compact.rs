/// Prompt used to create a handoff summary during local history compaction.
pub const SUMMARIZATION_PROMPT: &str = include_str!("../templates/compact/prompt.md");

/// Instruction appended to a generated compaction summary for the next model turn.
pub const SUMMARY_SUFFIX: &str = include_str!("../templates/compact/summary_suffix.md");

/// Legacy instruction that was prepended to summaries before [`SUMMARY_SUFFIX`] was introduced.
///
/// Keep this available so existing rollout entries can still be recognized and replayed.
pub const SUMMARY_PREFIX: &str = include_str!("../templates/compact/summary_prefix.md");
