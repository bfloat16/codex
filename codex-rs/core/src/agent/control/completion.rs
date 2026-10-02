use super::*;

#[derive(Clone, Copy)]
pub(super) struct PendingV2Task {
    parent_thread_id: ThreadId,
    unreceived_inputs: usize,
}

impl AgentControl {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "task registration and submission must stay atomic with input acknowledgement and completion"
    )]
    pub(super) async fn send_inter_agent_communication_after_capacity_check(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
        start_options: TurnStartOptions,
    ) -> CodexResult<String> {
        let parent_thread_id = if communication.trigger_turn {
            let thread = state.get_thread(agent_id).await?;
            (thread.multi_agent_version() == Some(MultiAgentVersion::V2))
                .then(|| thread.session_source.parent_thread_id())
                .flatten()
        } else {
            None
        };
        // Register work before submission so a fast child cannot complete before it is tracked.
        let mut pending = self.v2_pending_tasks.lock().await;
        let previous = parent_thread_id.and_then(|parent| {
            let previous = pending.get(&agent_id).copied();
            pending.insert(
                agent_id,
                PendingV2Task {
                    parent_thread_id: parent,
                    unreceived_inputs: previous.map_or(1, |task| task.unreceived_inputs + 1),
                },
            );
            previous
        });
        let result = self
            .submit_inter_agent_communication(
                agent_id,
                state,
                communication,
                context,
                start_options,
            )
            .await;
        if result.is_err() && parent_thread_id.is_some() {
            if let Some(previous) = previous {
                pending.insert(agent_id, previous);
            } else {
                pending.remove(&agent_id);
            }
        }
        result
    }

    pub(crate) async fn receive_inter_agent_communication(
        &self,
        session: &Arc<crate::session::session::Session>,
        sub_id: String,
        communication: InterAgentCommunication,
        start_options: TurnStartOptions,
    ) {
        let trigger_turn = communication.trigger_turn;
        session
            .input_queue
            .enqueue_mailbox_communication(communication, start_options)
            .await;
        crate::agent_communication::emit_agent_communication_receive(&sub_id);
        if trigger_turn {
            let mut pending = self.v2_pending_tasks.lock().await;
            if let Some(task) = pending.get_mut(&session.thread_id) {
                task.unreceived_inputs = task.unreceived_inputs.saturating_sub(1);
            }
        }
        if trigger_turn || session.has_outstanding_durable_sleep() {
            session
                .maybe_start_turn_for_pending_work_with_sub_id(sub_id)
                .await;
        }
    }

    pub(crate) async fn has_pending_v2_children(&self, parent_thread_id: ThreadId) -> bool {
        let pending = self.v2_pending_tasks.lock().await;
        let parent_path = self
            .state
            .agent_metadata_for_thread(parent_thread_id)
            .and_then(|metadata| metadata.agent_path);
        pending.values().any(|task| {
            task.parent_thread_id == parent_thread_id
                || parent_path.as_ref().is_some_and(|parent_path| {
                    let owner_path = self
                        .state
                        .agent_metadata_for_thread(task.parent_thread_id)
                        .and_then(|metadata| metadata.agent_path);
                    agent_matches_prefix(owner_path.as_ref(), parent_path)
                })
        })
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "completion checks and result enqueueing must stay atomic with new task registration"
    )]
    pub(crate) async fn deliver_v2_completion(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        status: &AgentStatus,
        mut communication: InterAgentCommunication,
        config: Config,
        mut start_options: TurnStartOptions,
    ) -> CodexResult<()> {
        if matches!(status, AgentStatus::Completed(_))
            && self.has_pending_v2_children(child_thread_id).await
        {
            return Ok(());
        }
        self.ensure_v2_agent_loaded(config, parent_thread_id, /*parent*/ None)
            .await?;
        let manager = self.upgrade()?;
        let parent = manager.get_thread(parent_thread_id).await?;
        let mut pending = self.v2_pending_tasks.lock().await;
        if pending
            .get(&child_thread_id)
            .is_some_and(|task| task.unreceived_inputs > 0)
        {
            return Ok(());
        }
        if let Ok(child) = manager.get_thread(child_thread_id).await
            && (child.session.active_turn.lock().await.is_some()
                || child
                    .session
                    .input_queue
                    .has_trigger_turn_mailbox_items()
                    .await)
        {
            return Ok(());
        }
        let completed_task = pending.remove(&child_thread_id).is_some();
        communication.trigger_turn = completed_task && !parent.session.is_interrupted();
        if communication.trigger_turn
            && let Some(owner) = parent.session_source.parent_thread_id()
        {
            pending.entry(parent_thread_id).or_insert(PendingV2Task {
                parent_thread_id: owner,
                unreceived_inputs: 0,
            });
        }
        start_options.cyber_access_program = parent
            .session
            .reference_context_item()
            .await
            .and_then(|context| context.cyber_access_program);
        let context =
            AgentCommunicationContext::new(AgentCommunicationKind::Result, child_thread_id);
        let communication_id = Uuid::new_v4().to_string();
        crate::agent_communication::emit_agent_communication_send(
            &communication_id,
            &context,
            &communication,
            parent_thread_id,
        );
        // Queue results before allowing any parent to resume; the scheduler checks pending work.
        parent
            .session
            .input_queue
            .enqueue_mailbox_communication(communication, start_options)
            .await;
        crate::agent_communication::emit_agent_communication_receive(&communication_id);
        drop(pending);
        let mut ancestor = Some(parent);
        while let Some(thread) = ancestor {
            if !thread.session.is_interrupted() {
                thread.session.maybe_start_turn_for_pending_work().await;
            }
            ancestor = match thread.session_source.parent_thread_id() {
                Some(parent_thread_id) => manager.get_thread(parent_thread_id).await.ok(),
                None => None,
            };
        }
        Ok(())
    }
}
