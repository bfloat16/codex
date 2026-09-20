use codex_app_server_protocol::CommandAction;
use codex_app_server_protocol::CommandExecutionSource;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::FileUpdateChange;
use codex_app_server_protocol::PatchApplyStatus;
use codex_app_server_protocol::PatchChangeKind;
use codex_app_server_protocol::ThreadItem;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::*;
use crate::conversation_viewport::ConversationViewport;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::ToolActivity;
use crate::terminal_hyperlinks::visible_lines;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;

#[test]
fn paginated_commands_keep_tool_activity_and_native_exec_rendering() {
    let cwd = test_path_buf("/tmp/project").abs();
    let read_path = test_path_buf("/tmp/project/src/lib.rs").abs();
    let command = |id: &str, command: &str, command_actions: Vec<CommandAction>, output: &str| {
        ThreadItem::CommandExecution {
            id: id.to_string(),
            plugin_id: None,
            script_path: None,
            command: command.to_string(),
            cwd: cwd.clone().into(),
            process_id: None,
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::Completed,
            command_actions,
            aggregated_output: Some(output.to_string()),
            exit_code: Some(0),
            duration_ms: Some(10),
        }
    };
    let cells = thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &cwd,
        [
            command(
                "read",
                "cat src/lib.rs",
                vec![CommandAction::Read {
                    command: "cat src/lib.rs".to_string(),
                    name: "lib.rs".to_string(),
                    path: read_path.into(),
                }],
                "persisted source text",
            ),
            command("shell", "echo paginated", Vec::new(), "paginated"),
        ],
        RawReasoningVisibility::Hidden,
        /*config*/ None,
    );

    assert_eq!(
        cells
            .iter()
            .map(|cell| cell.tool_activity())
            .collect::<Vec<_>>(),
        vec![
            Some(ToolActivity {
                call_count: 1,
                read_files: 1,
                ..ToolActivity::default()
            }),
            Some(ToolActivity {
                call_count: 1,
                shell_commands: 1,
                ..ToolActivity::default()
            }),
        ]
    );

    let read_detail = visible_lines(cells[0].tool_group_detail_lines(/*width*/ 80))
        .into_iter()
        .flat_map(|line| line.spans)
        .map(|span| span.content.into_owned())
        .collect::<String>();
    let shell_detail = visible_lines(cells[1].tool_group_detail_lines(/*width*/ 80))
        .into_iter()
        .flat_map(|line| line.spans)
        .map(|span| span.content.into_owned())
        .collect::<String>();
    assert!(read_detail.contains("● Explored"));
    assert!(read_detail.contains("Read lib.rs"));
    assert!(!read_detail.contains("persisted source text"));
    assert!(shell_detail.contains("● Ran echo paginated"));
    assert!(shell_detail.contains("paginated"));

    let mut viewport = ConversationViewport::new(
        cells,
        HistoryRenderMode::Rich,
        crate::keymap::RuntimeKeymap::defaults().pager,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 48, /*height*/ 3,
    );
    let mut buffer = Buffer::empty(area);
    viewport.render(area, &mut buffer);
    let rendered = (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(rendered, @"
    Read 1 file, ran 1 shell command
    └ Ran echo paginated
    ");
}

#[test]
fn paginated_file_changes_use_native_patch_rendering_without_folding() {
    let cwd = test_path_buf("/tmp/project").abs();
    let cells = thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &cwd,
        [ThreadItem::FileChange {
            id: "patch".to_string(),
            changes: vec![FileUpdateChange {
                path: "src/new.rs".to_string(),
                kind: PatchChangeKind::Add,
                diff: "fn added() {}\n".to_string(),
            }],
            status: PatchApplyStatus::Completed,
        }],
        RawReasoningVisibility::Hidden,
        /*config*/ None,
    );

    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].tool_activity(), None);
    let rendered = cells[0]
        .display_lines(/*width*/ 80)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(rendered, @r###"
● Added src/new.rs (+1 -0)
    1 +fn added() {}
"###);
}
