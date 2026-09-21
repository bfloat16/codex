//! Keep in-flight tool entries addressable after they move out of the active tail.

use super::*;
use crate::history_cell::LiveToolCell;

impl ChatWidget {
    pub(super) fn retain_running_tool(
        &mut self,
        cell: Box<dyn HistoryCell>,
    ) -> Box<dyn HistoryCell> {
        let ids: Vec<String> = if let Some(exec) = cell.as_any().downcast_ref::<ExecCell>() {
            exec.iter_calls()
                .filter(|call| {
                    call.duration.is_none()
                        || self.interrupted_unified_exec_calls.contains(&call.call_id)
                })
                .map(|call| call.call_id.clone())
                .collect()
        } else if let Some(mcp) = cell.as_any().downcast_ref::<McpToolCallCell>() {
            mcp.tool_group_preview_is_active()
                .then(|| mcp.call_id().to_string())
                .into_iter()
                .collect()
        } else if let Some(search) = cell.as_any().downcast_ref::<WebSearchCell>() {
            search
                .tool_group_preview_is_active()
                .then(|| search.call_id().to_string())
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        if ids.is_empty() {
            return cell;
        }
        let shared = LiveToolCell::new(cell);
        for id in ids {
            self.transcript
                .running_tool_cells
                .insert(id, shared.clone());
        }
        Box::new(shared)
    }

    pub(super) fn finalize_retained_tools(&mut self) {
        for shared in self.transcript.running_tool_cells.values() {
            let mut cell = shared.lock();
            if let Some(exec) = cell.as_any_mut().downcast_mut::<ExecCell>() {
                exec.mark_failed();
            } else if let Some(mcp) = cell.as_any_mut().downcast_mut::<McpToolCallCell>() {
                mcp.mark_failed();
            } else if let Some(search) = cell.as_any_mut().downcast_mut::<WebSearchCell>() {
                search.complete();
            }
        }
        self.transcript
            .running_tool_cells
            .retain(|id, _| self.interrupted_unified_exec_calls.contains(id));
    }
}
