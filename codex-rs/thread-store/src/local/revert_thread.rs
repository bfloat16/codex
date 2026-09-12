use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;

use super::LocalThreadStore;
use super::thread_history;
use super::thread_rollout_resolver;
use crate::RevertThreadParams;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

/// Revert an unloaded paginated thread in its existing rollout file.
///
/// The selected rollout is truncated at the target turn's durable byte offset and its SQLite
/// projection is rebuilt from that prefix. Explicit `/fork` remains the operation that creates a
/// new rollout file.
pub(super) async fn revert(
    store: &LocalThreadStore,
    params: RevertThreadParams,
) -> ThreadStoreResult<()> {
    let RevertThreadParams {
        thread_id,
        before_turn_id,
        multi_agent_version,
    } = params;
    let state_db = store
        .state_db()
        .await
        .ok_or(ThreadStoreError::Unsupported {
            operation: "revert_thread",
        })?;
    let _lifecycle_guard = store.live_writer_locks.lock_lifecycle(thread_id).await;
    let _live_writer_guard = store.live_writer_locks.lock(thread_id).await;
    store.ensure_live_recorder_absent(thread_id).await?;
    let _writer_lock = store.writer_lock_coordinator.acquire(thread_id)?;

    // Resolution may return a compressed sibling. Keep SQLite's exact stored path for the CAS.
    let expected_sqlite_path = state_db
        .get_thread(thread_id)
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to read thread metadata for {thread_id}: {err}"),
        })?
        .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?
        .rollout_path;
    let current_rollout = thread_rollout_resolver::resolve_current(store, thread_id)
        .await?
        .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?;
    let source_path = current_rollout.path;
    let mut source_meta_line = codex_rollout::read_session_meta_line(source_path.as_path())
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to read current paginated rollout {}: {err}",
                source_path.display()
            ),
        })?;
    let should_update_multi_agent_version = multi_agent_version.is_some()
        && multi_agent_version != source_meta_line.meta.multi_agent_version;
    source_meta_line.meta.multi_agent_version =
        multi_agent_version.or(source_meta_line.meta.multi_agent_version);
    let source_meta = source_meta_line.meta.clone();
    if source_meta.id != current_rollout.thread_id {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("current rollout for {thread_id} belongs to another thread"),
        });
    }
    if source_meta.history_mode != ThreadHistoryMode::Paginated {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("thread {thread_id} does not use paginated history"),
        });
    }

    let lineage = store.resolve_rollout_lineage(thread_id).await?;
    for segment in lineage.segments() {
        super::thread_history_materialization::materialize_to_sqlite(
            store,
            segment.rollout_id(),
            segment.rollout_path.as_path(),
        )
        .await?;
    }
    let pool = store.thread_history_db().await?;
    let target = thread_history::find_source_turn(pool, &lineage, before_turn_id.as_str()).await?;
    let Some(truncate_at) = target.rollout_byte_offset else {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("turn {before_turn_id} does not have a persisted start boundary"),
        });
    };
    let truncate_at = u64::try_from(truncate_at).map_err(|_| ThreadStoreError::Internal {
        message: format!("turn {before_turn_id} has an invalid byte offset"),
    })?;

    let retained = if target.rollout_id == current_rollout.rollout_id {
        None
    } else {
        Some(retained_rollout_lines(&lineage, target.rollout_id, target.rollout_ordinal).await?)
    };
    let source_path = codex_rollout::materialize_rollout_for_reference(source_path.as_path())
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to materialize rollout for revert: {err}"),
        })?;
    let mut projection_truncated = false;
    if target.rollout_id == current_rollout.rollout_id {
        if should_update_multi_agent_version {
            let mut retained_bytes = tokio::fs::read(source_path.as_path())
                .await
                .map_err(thread_store_io_error)?;
            let truncate_at =
                usize::try_from(truncate_at).map_err(|_| ThreadStoreError::Internal {
                    message: format!("turn {before_turn_id} has an invalid byte offset"),
                })?;
            retained_bytes.truncate(truncate_at);
            let first_line_end = retained_bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .ok_or_else(|| ThreadStoreError::Internal {
                    message: "rollout is missing its session metadata line".to_string(),
                })?;
            let metadata = RolloutLine {
                timestamp: source_meta.timestamp.clone(),
                ordinal: Some(0),
                item: RolloutItem::SessionMeta(source_meta_line),
            };
            let mut bytes = serde_json::to_vec(&metadata).map_err(serde_store_error)?;
            bytes.push(b'\n');
            bytes.extend_from_slice(&retained_bytes[first_line_end..]);
            tokio::fs::write(source_path.as_path(), bytes)
                .await
                .map_err(thread_store_io_error)?;
        } else {
            projection_truncated = thread_history::truncate_projection(
                store,
                current_rollout.rollout_id,
                truncate_at,
                target.rollout_ordinal,
            )
            .await?;
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(source_path.as_path())
                .await
                .map_err(thread_store_io_error)?;
            file.set_len(truncate_at)
                .await
                .map_err(thread_store_io_error)?;
            drop(file);
        }
    } else {
        let retained = retained.ok_or_else(|| ThreadStoreError::Internal {
            message: "missing retained rollout lines for inherited rollback".to_string(),
        })?;
        let mut meta = source_meta;
        meta.history_base = None;
        meta.subagent_history_start_ordinal = None;
        meta.forked_from_ordinal_exclusive = meta.forked_from_id.is_some().then_some(
            u64::try_from(target.rollout_ordinal).map_err(|_| ThreadStoreError::Internal {
                message: format!("turn {before_turn_id} has an invalid rollout ordinal"),
            })?,
        );
        let metadata = RolloutLine {
            timestamp: meta.timestamp.clone(),
            ordinal: Some(0),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta,
                git: source_meta_line.git,
            }),
        };
        let mut bytes = serde_json::to_vec(&metadata).map_err(serde_store_error)?;
        bytes.push(b'\n');
        for line in retained {
            let mut line_bytes = serde_json::to_vec(&line).map_err(serde_store_error)?;
            line_bytes.push(b'\n');
            bytes.extend(line_bytes);
        }
        tokio::fs::write(source_path.as_path(), bytes)
            .await
            .map_err(thread_store_io_error)?;
    }
    if !projection_truncated {
        thread_history::reset_projection(store, current_rollout.rollout_id).await?;
        super::thread_history_materialization::materialize_to_sqlite(
            store,
            current_rollout.rollout_id,
            source_path.as_path(),
        )
        .await?;
    }

    if expected_sqlite_path != source_path {
        let replaced = state_db
            .replace_rollout_path_if_current(
                thread_id,
                expected_sqlite_path.as_path(),
                source_path.as_path(),
            )
            .await
            .map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to update reverted rollout path: {err}"),
            })?;
        if !replaced {
            return Err(ThreadStoreError::Conflict {
                message: format!("thread {thread_id} changed while it was being reverted"),
            });
        }
    }
    Ok(())
}

async fn retained_rollout_lines(
    lineage: &super::rollout_lineage::RolloutLineage,
    target_rollout_id: ThreadId,
    target_ordinal: i64,
) -> ThreadStoreResult<Vec<RolloutLine>> {
    let target_ordinal = u64::try_from(target_ordinal).map_err(|_| ThreadStoreError::Internal {
        message: "target turn has a negative rollout ordinal".to_string(),
    })?;
    let mut lines = Vec::new();
    for segment in lineage.segments() {
        if segment.rollout_id() == target_rollout_id {
            let mut reader =
                codex_rollout::open_rollout_line_reader(segment.rollout_path.as_path())
                    .await
                    .map_err(thread_store_io_error)?;
            while let Some(line) = reader.next_line().await.map_err(thread_store_io_error)? {
                let line = codex_rollout::parse_rollout_line(&line).map_err(|err| {
                    ThreadStoreError::Internal {
                        message: format!(
                            "failed to parse rollout line {}: {err}",
                            segment.rollout_path.display()
                        ),
                    }
                })?;
                if line
                    .ordinal
                    .is_some_and(|ordinal| ordinal >= target_ordinal)
                {
                    break;
                }
                if is_line_in_segment(&line, segment)
                    && !matches!(&line.item, RolloutItem::SessionMeta(_))
                {
                    lines.push(line);
                }
            }
            break;
        }

        let mut reader = codex_rollout::open_rollout_line_reader(segment.rollout_path.as_path())
            .await
            .map_err(thread_store_io_error)?;
        while let Some(line) = reader.next_line().await.map_err(thread_store_io_error)? {
            let line = codex_rollout::parse_rollout_line(&line).map_err(|err| {
                ThreadStoreError::Internal {
                    message: format!(
                        "failed to parse rollout line {}: {err}",
                        segment.rollout_path.display()
                    ),
                }
            })?;
            if is_line_in_segment(&line, segment)
                && !matches!(&line.item, RolloutItem::SessionMeta(_))
            {
                lines.push(line);
            }
        }
    }
    Ok(lines)
}

fn is_line_in_segment(
    line: &RolloutLine,
    segment: &super::rollout_lineage::RolloutLineageSegment,
) -> bool {
    let Some(ordinal) = line.ordinal else {
        return false;
    };
    ordinal >= segment.start_ordinal()
        && segment
            .end_ordinal()
            .is_none_or(|end_ordinal| ordinal < end_ordinal)
}

fn serde_store_error(err: serde_json::Error) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: err.to_string(),
    }
}

fn thread_store_io_error(err: std::io::Error) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: err.to_string(),
    }
}

#[cfg(test)]
#[path = "revert_thread_tests.rs"]
mod tests;
