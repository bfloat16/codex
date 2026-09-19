use super::BacktrackRollbackTarget;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::UserMessage;
use crate::chatwidget::mention_bindings_from_user_inputs;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use color_eyre::eyre::Result;
use color_eyre::eyre::bail;

#[derive(Clone, Copy)]
enum RollbackEntry<'a> {
    PersistedPrompt {
        turn_index: usize,
        content: &'a [UserInput],
    },
    UnpersistedPrompt {
        turn_index: usize,
    },
}

impl RollbackEntry<'_> {
    fn turn_index(self) -> usize {
        match self {
            Self::PersistedPrompt { turn_index, .. } | Self::UnpersistedPrompt { turn_index } => {
                turn_index
            }
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct AlignmentScore {
    exact_matches: usize,
    position_sum: usize,
}

impl AlignmentScore {
    fn with_match(self, entry_index: usize, exact: bool) -> Self {
        Self {
            exact_matches: self.exact_matches.saturating_add(usize::from(exact)),
            position_sum: self.position_sum.saturating_add(entry_index),
        }
    }
}

#[derive(Clone, Copy, Default)]
enum AlignmentStep {
    #[default]
    Unavailable,
    Match,
    SkipUnpersisted,
}

/// Find the persisted turn boundary for a prompt selected from the TUI transcript.
///
/// UserPromptSubmit hooks can stop a turn before its user message is persisted. The TUI has
/// already rendered that prompt, while app-server history contains only an empty turn boundary.
/// Alignment therefore treats user-less turns as optional wildcard slots, preferring exact
/// persisted prompt matches and the newest valid alignment. This also skips unrelated user-less
/// turns such as compactions without shifting later prompt selections.
pub(crate) fn backtrack_rollback_target(
    turns: &[Turn],
    transcript_prompts: &[UserMessage],
    newer_user_messages: usize,
    prompt: &mut UserMessage,
) -> Result<BacktrackRollbackTarget> {
    let Some(selected_prompt_index) = transcript_prompts
        .len()
        .checked_sub(newer_user_messages.saturating_add(/*rhs*/ 1))
    else {
        bail!("the selected prompt was not found in the persisted thread");
    };
    if !prompts_match(&transcript_prompts[selected_prompt_index], prompt) {
        bail!("the selected transcript prompt no longer matches the persisted thread");
    }

    let entries = rollback_entries(turns);
    let Some(alignment) = align_transcript_prompts(transcript_prompts, &entries) else {
        bail!("the selected transcript prompt no longer matches the persisted thread");
    };
    let entry = entries[alignment[selected_prompt_index]];
    let turn_index = entry.turn_index();
    let turn = &turns[turn_index];
    if matches!(turn.status, TurnStatus::InProgress) {
        bail!("the selected prompt belongs to a turn that is still in progress");
    }

    if let RollbackEntry::PersistedPrompt { content, .. } = entry {
        let display = ChatWidget::user_message_display_from_inputs(content);
        prompt.mention_bindings = mention_bindings_from_user_inputs(content, &display.message);
    }
    let legacy_num_turns = turns[turn_index..]
        .iter()
        .flat_map(|turn| &turn.items)
        .filter(|item| matches!(item, ThreadItem::UserMessage { .. }))
        .count();
    let Ok(legacy_num_turns) = u32::try_from(legacy_num_turns) else {
        bail!("the selected prompt requires rolling back too many turns");
    };
    Ok(BacktrackRollbackTarget {
        before_turn_id: turn.id.clone(),
        legacy_num_turns,
        removed_turn_ids: turns[turn_index..]
            .iter()
            .map(|turn| turn.id.clone())
            .collect(),
    })
}

fn rollback_entries(turns: &[Turn]) -> Vec<RollbackEntry<'_>> {
    let mut entries = Vec::new();
    let mut review_mode = false;
    for (turn_index, turn) in turns.iter().enumerate() {
        let hidden_nested_review_turn = turn_index
            .checked_sub(/*rhs*/ 1)
            .and_then(|index| turns.get(index))
            .is_some_and(|previous| is_hidden_nested_review_turn(previous, turn));
        let mut has_user_message = false;
        for item in &turn.items {
            let content = match item {
                ThreadItem::EnteredReviewMode { .. } => {
                    review_mode = true;
                    continue;
                }
                ThreadItem::ExitedReviewMode { .. } => {
                    review_mode = false;
                    continue;
                }
                ThreadItem::UserMessage { content, .. } => {
                    has_user_message = true;
                    content
                }
                _ => continue,
            };
            if review_mode || hidden_nested_review_turn {
                continue;
            }

            let display = ChatWidget::user_message_display_from_inputs(content);
            if display.message.trim().is_empty()
                && display.text_elements.is_empty()
                && display.local_images.is_empty()
                && display.remote_image_urls.is_empty()
            {
                continue;
            }
            entries.push(RollbackEntry::PersistedPrompt {
                turn_index,
                content,
            });
        }
        if !has_user_message {
            entries.push(RollbackEntry::UnpersistedPrompt { turn_index });
        }
    }
    entries
}

fn align_transcript_prompts(
    prompts: &[UserMessage],
    entries: &[RollbackEntry<'_>],
) -> Option<Vec<usize>> {
    if prompts.is_empty() {
        return Some(Vec::new());
    }

    // scores[i][j] aligns the first i transcript prompts to a suffix ending at entry j. A
    // persisted prompt cannot be skipped once alignment has started; a user-less turn can either
    // represent a locally rendered, hook-blocked prompt or be skipped as unrelated history. The
    // alignment may end before the newest persisted entry because an Esc interruption can persist
    // its tail before the corresponding TUI transcript event is rendered.
    let mut previous_scores = vec![Some(AlignmentScore::default()); entries.len() + 1];
    let mut steps = vec![vec![AlignmentStep::Unavailable; entries.len() + 1]; prompts.len() + 1];
    for prompt_index in 1..=prompts.len() {
        let mut current_scores = vec![None; entries.len() + 1];
        for entry_index in 1..=entries.len() {
            let entry = entries[entry_index - 1];
            match entry {
                RollbackEntry::PersistedPrompt { content, .. } => {
                    if prompt_matches_content(&prompts[prompt_index - 1], content)
                        && let Some(score) = previous_scores[entry_index - 1]
                    {
                        current_scores[entry_index] =
                            Some(score.with_match(entry_index - 1, /*exact*/ true));
                        steps[prompt_index][entry_index] = AlignmentStep::Match;
                    }
                }
                RollbackEntry::UnpersistedPrompt { .. } => {
                    let skipped = current_scores[entry_index - 1];
                    let matched = previous_scores[entry_index - 1]
                        .map(|score| score.with_match(entry_index - 1, /*exact*/ false));
                    if matched > skipped {
                        current_scores[entry_index] = matched;
                        steps[prompt_index][entry_index] = AlignmentStep::Match;
                    } else if skipped.is_some() {
                        current_scores[entry_index] = skipped;
                        steps[prompt_index][entry_index] = AlignmentStep::SkipUnpersisted;
                    }
                }
            }
        }
        previous_scores = current_scores;
    }
    let entry_index = previous_scores
        .iter()
        .enumerate()
        .filter_map(|(entry_index, score)| score.map(|score| (entry_index, score)))
        .max_by_key(|(_, score)| *score)?
        .0;

    let mut alignment = vec![0; prompts.len()];
    let mut prompt_index = prompts.len();
    let mut entry_index = entry_index;
    while prompt_index > 0 {
        match steps[prompt_index][entry_index] {
            AlignmentStep::Match => {
                alignment[prompt_index - 1] = entry_index - 1;
                prompt_index -= 1;
                entry_index -= 1;
            }
            AlignmentStep::SkipUnpersisted => entry_index -= 1,
            AlignmentStep::Unavailable => return None,
        }
    }
    Some(alignment)
}

fn prompt_matches_content(prompt: &UserMessage, content: &[UserInput]) -> bool {
    let display = ChatWidget::user_message_display_from_inputs(content);
    prompt.text == display.message
        && prompt.text_elements == display.text_elements
        && prompt.remote_image_urls == display.remote_image_urls
        && prompt
            .local_images
            .iter()
            .map(|image| &image.path)
            .eq(display.local_images.iter())
}

fn prompts_match(left: &UserMessage, right: &UserMessage) -> bool {
    left.text == right.text
        && left.text_elements == right.text_elements
        && left.local_images == right.local_images
        && left.remote_image_urls == right.remote_image_urls
}

/// Returns whether a turn is the reconstructed inline-review child with duplicated prompt inputs.
pub(crate) fn is_hidden_nested_review_turn(previous: &Turn, turn: &Turn) -> bool {
    if previous.status != TurnStatus::Completed
        || turn.status != TurnStatus::Interrupted
        || turn.completed_at.is_some()
        || !previous
            .items
            .iter()
            .any(|item| matches!(item, ThreadItem::EnteredReviewMode { .. }))
        || !previous
            .items
            .iter()
            .any(|item| matches!(item, ThreadItem::ExitedReviewMode { .. }))
    {
        return false;
    }

    let mut user_messages = turn.items.iter().filter_map(|item| match item {
        ThreadItem::UserMessage { content, .. } => Some(content),
        _ => None,
    });
    matches!(
        (
            user_messages.next(),
            user_messages.next(),
            user_messages.next(),
        ),
        (Some(first), Some(second), None) if first == second
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bottom_pane::MentionBinding;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn turn(turn_id: &str, status: TurnStatus, user_messages: usize) -> Turn {
        Turn {
            id: turn_id.to_string(),
            items: (0..user_messages)
                .map(|index| ThreadItem::UserMessage {
                    id: format!("user-{index}"),
                    client_id: None,
                    content: vec![UserInput::Text {
                        text: format!("{turn_id}-prompt-{index}"),
                        text_elements: Vec::new(),
                    }],
                })
                .collect(),
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            status,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }
    }

    fn prompt(text: &str) -> UserMessage {
        UserMessage {
            text: text.to_string(),
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        }
    }

    fn target(
        turns: &[Turn],
        transcript: &[UserMessage],
        newer_user_messages: usize,
        selected: &str,
    ) -> Result<BacktrackRollbackTarget> {
        backtrack_rollback_target(
            turns,
            transcript,
            newer_user_messages,
            &mut prompt(selected),
        )
    }

    #[test]
    fn resolves_first_and_later_prompts_around_userless_turns() {
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            turn(
                "turn-compaction",
                TurnStatus::Completed,
                /*user_messages*/ 0,
            ),
            turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1),
        ];
        let transcript = vec![prompt("turn-1-prompt-0"), prompt("turn-2-prompt-0")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 1,
                "turn-1-prompt-0"
            )
            .expect("first prompt should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-1".to_string(),
                legacy_num_turns: 2,
                removed_turn_ids: vec![
                    "turn-1".to_string(),
                    "turn-compaction".to_string(),
                    "turn-2".to_string(),
                ],
            }
        );
        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-2-prompt-0"
            )
            .expect("later prompt should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["turn-2".to_string()],
            }
        );
    }

    #[test]
    fn resolves_hook_blocked_prompt_to_userless_turn_boundary() {
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            turn(
                "turn-hook-blocked",
                TurnStatus::Completed,
                /*user_messages*/ 0,
            ),
        ];
        let transcript = vec![prompt("turn-1-prompt-0"), prompt("123456 example")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "123456 example",
            )
            .expect("hook-blocked prompt should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-hook-blocked".to_string(),
                legacy_num_turns: 0,
                removed_turn_ids: vec!["turn-hook-blocked".to_string()],
            }
        );
        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 1,
                "turn-1-prompt-0",
            )
            .expect("prompt before a hook-blocked turn should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-1".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["turn-1".to_string(), "turn-hook-blocked".to_string(),],
            }
        );
    }

    #[test]
    fn aligns_duplicate_hook_blocked_prompt_without_stealing_persisted_match() {
        let turns = vec![
            turn(
                "turn-prefix",
                TurnStatus::Completed,
                /*user_messages*/ 1,
            ),
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            turn(
                "turn-hook-blocked",
                TurnStatus::Completed,
                /*user_messages*/ 0,
            ),
        ];
        let transcript = vec![prompt("turn-1-prompt-0"), prompt("turn-1-prompt-0")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-1-prompt-0",
            )
            .expect("duplicate blocked prompt should use the empty turn"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-hook-blocked".to_string(),
                legacy_num_turns: 0,
                removed_turn_ids: vec!["turn-hook-blocked".to_string()],
            }
        );
    }

    #[test]
    fn resolves_failed_compact_after_earlier_steers() {
        let mut earlier_compact = turn(
            "turn-earlier-compact",
            TurnStatus::Completed,
            /*user_messages*/ 1,
        );
        let ThreadItem::UserMessage { content, .. } = &mut earlier_compact.items[0] else {
            panic!("expected user message")
        };
        *content = vec![UserInput::Text {
            text: "/compact".to_string(),
            text_elements: Vec::new(),
        }];
        let mut failed_compact = turn(
            "turn-failed-compact",
            TurnStatus::Failed,
            /*user_messages*/ 1,
        );
        let ThreadItem::UserMessage { content, .. } = &mut failed_compact.items[0] else {
            panic!("expected user message")
        };
        *content = vec![UserInput::Text {
            text: "/compact".to_string(),
            text_elements: Vec::new(),
        }];
        let turns = vec![
            turn(
                "turn-with-steers",
                TurnStatus::Interrupted,
                /*user_messages*/ 3,
            ),
            earlier_compact,
            turn(
                "turn-after-compact",
                TurnStatus::Completed,
                /*user_messages*/ 1,
            ),
            failed_compact,
        ];
        let transcript = vec![prompt("/compact")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "/compact"
            )
            .expect("latest failed compact should resolve independently of the history prefix"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-failed-compact".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["turn-failed-compact".to_string()],
            }
        );
    }

    #[test]
    fn resolves_mid_turn_steers_at_turn_boundary() {
        let turns = vec![turn(
            "turn-1",
            TurnStatus::Completed,
            /*user_messages*/ 2,
        )];
        let transcript = vec![prompt("turn-1-prompt-0"), prompt("turn-1-prompt-1")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-1-prompt-1",
            )
            .expect("a steer should roll back at its containing turn boundary"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-1".to_string(),
                legacy_num_turns: 2,
                removed_turn_ids: vec!["turn-1".to_string()],
            }
        );
    }

    #[test]
    fn resolves_model_output_interruption_from_persisted_prompt() {
        let mut interrupted = turn("turn-1", TurnStatus::Interrupted, /*user_messages*/ 1);
        interrupted.items.push(ThreadItem::AgentMessage {
            id: "agent-1".to_string(),
            text: "partial answer".to_string(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        });
        let transcript = vec![prompt("turn-1-prompt-0")];

        assert_eq!(
            target(
                &[interrupted],
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-1-prompt-0",
            )
            .expect("an interrupted model-output turn should remain rewindable"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-1".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["turn-1".to_string()],
            }
        );
    }

    #[test]
    fn resolves_snapshot_before_late_interrupted_turn() {
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            turn("turn-2", TurnStatus::Interrupted, /*user_messages*/ 1),
        ];
        let transcript = vec![prompt("turn-1-prompt-0")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-1-prompt-0",
            )
            .expect("a persisted interrupt tail should not invalidate the transcript snapshot"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-1".to_string(),
                legacy_num_turns: 2,
                removed_turn_ids: vec!["turn-1".to_string(), "turn-2".to_string()],
            }
        );
    }

    #[test]
    fn rejects_in_progress_missing_and_stale_prompts() {
        let turns = vec![turn(
            "turn-1",
            TurnStatus::InProgress,
            /*user_messages*/ 1,
        )];
        let transcript = vec![prompt("turn-1-prompt-0")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-1-prompt-0",
            )
            .expect_err("in-progress prompt cannot be rolled back")
            .to_string(),
            "the selected prompt belongs to a turn that is still in progress"
        );
        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 1,
                "missing prompt",
            )
            .expect_err("missing prompt cannot be rolled back")
            .to_string(),
            "the selected prompt was not found in the persisted thread"
        );

        let completed_turns = vec![turn(
            "turn-1",
            TurnStatus::Completed,
            /*user_messages*/ 1,
        )];
        assert_eq!(
            target(
                &completed_turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "different prompt",
            )
            .expect_err("a stale transcript prompt cannot be rolled back")
            .to_string(),
            "the selected transcript prompt no longer matches the persisted thread"
        );
    }

    #[test]
    fn skips_hidden_review_prompts() {
        let mut review_turn = turn(
            "turn-review",
            TurnStatus::Completed,
            /*user_messages*/ 1,
        );
        review_turn.items.insert(
            /*index*/ 0,
            ThreadItem::EnteredReviewMode {
                id: "review-start".to_string(),
                review: "changes against main".to_string(),
            },
        );
        review_turn.items.push(ThreadItem::ExitedReviewMode {
            id: "review-end".to_string(),
            review: "review complete".to_string(),
        });
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            review_turn,
            turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1),
        ];
        let transcript = vec![prompt("turn-1-prompt-0"), prompt("turn-2-prompt-0")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-2-prompt-0",
            )
            .expect("the visible prompt after review should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["turn-2".to_string()],
            }
        );
    }

    #[test]
    fn skips_hidden_nested_review_prompts() {
        let review_hint = "current changes";
        let review_prompt =
            "Review the current code changes (staged, unstaged, and untracked files).";
        let review_turn = Turn {
            items: vec![
                ThreadItem::EnteredReviewMode {
                    id: "review-start".to_string(),
                    review: review_hint.to_string(),
                },
                ThreadItem::ExitedReviewMode {
                    id: "review-end".to_string(),
                    review: "review complete".to_string(),
                },
            ],
            ..turn(
                "turn-review",
                TurnStatus::Completed,
                /*user_messages*/ 0,
            )
        };
        let review_child_turn = Turn {
            items: (0..2)
                .map(|index| ThreadItem::UserMessage {
                    id: format!("review-prompt-{index}"),
                    client_id: None,
                    content: vec![UserInput::Text {
                        text: review_prompt.to_string(),
                        text_elements: Vec::new(),
                    }],
                })
                .collect(),
            ..turn(
                "turn-review-child",
                TurnStatus::Interrupted,
                /*user_messages*/ 0,
            )
        };
        let interrupted_steered_turn = Turn {
            items: review_child_turn.items.clone(),
            completed_at: Some(1),
            ..turn(
                "turn-interrupted-steer",
                TurnStatus::Interrupted,
                /*user_messages*/ 0,
            )
        };
        assert!(!is_hidden_nested_review_turn(
            &review_turn,
            &interrupted_steered_turn,
        ));
        let turns = vec![
            review_turn,
            review_child_turn,
            turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1),
        ];
        let transcript = vec![prompt("turn-2-prompt-0")];

        assert_eq!(
            target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                "turn-2-prompt-0",
            )
            .expect("the visible prompt after a nested review should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["turn-2".to_string()],
            }
        );
    }

    #[test]
    fn restores_canonical_mention_bindings() {
        let mut selected_turn = turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1);
        selected_turn.items = vec![ThreadItem::UserMessage {
            id: "selected-prompt".to_string(),
            client_id: None,
            content: vec![
                UserInput::Text {
                    text: "use $skill @sample $google-calendar".to_string(),
                    text_elements: Vec::new(),
                },
                UserInput::Skill {
                    name: "skill".to_string(),
                    path: PathBuf::from("/tmp/skills/skill/SKILL.md"),
                },
                UserInput::Mention {
                    name: "Sample Plugin".to_string(),
                    path: "plugin://sample@test".to_string(),
                },
                UserInput::Mention {
                    name: "Google Calendar".to_string(),
                    path: "app://google_calendar".to_string(),
                },
            ],
        }];
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            selected_turn,
        ];
        let transcript = vec![prompt("use $skill @sample $google-calendar")];
        let mut selected_prompt = prompt("use $skill @sample $google-calendar");

        assert_eq!(
            backtrack_rollback_target(
                &turns,
                &transcript,
                /*newer_user_messages*/ 0,
                &mut selected_prompt,
            )
            .expect("the selected prompt should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["turn-2".to_string()],
            }
        );
        assert_eq!(
            selected_prompt.mention_bindings,
            vec![
                MentionBinding {
                    sigil: '$',
                    mention: "skill".to_string(),
                    path: "/tmp/skills/skill/SKILL.md".to_string(),
                },
                MentionBinding {
                    sigil: '@',
                    mention: "sample".to_string(),
                    path: "plugin://sample@test".to_string(),
                },
                MentionBinding {
                    sigil: '$',
                    mention: "google-calendar".to_string(),
                    path: "app://google_calendar".to_string(),
                },
            ]
        );
    }
}
