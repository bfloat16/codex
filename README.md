# Compile

```shell
git clone https://github.com/bfloat16/codex.git
cd codex/codex-rs

rustup component add rustfmt clippy
cargo install --locked just
cargo install --locked cargo-nextest

# Windows
.\scripts\build-windows.ps1

# macOS or Linux
bash ./scripts/build-unix.sh
```

# New Features

## A more capable terminal workspace

- An owned full-screen terminal keeps the conversation viewport available for history navigation, mouse-wheel scrolling, text selection, and copy-friendly raw mode.
- The persistent `/diff` panel docks beside the conversation, lists changed and untracked files, supports navigation and refresh, and preserves the selected file while the working tree changes.
- File changes and tool output are grouped and folded independently, with live activity and clearer waiting states for long-running background terminals.
- Permission mode, plan mode, model, and provider controls can be changed during an active session without losing the current conversation context. `/provider` switches among configured providers for the current session.

## Rewind and recovery

- A staged, Claude-style rewind picker lives below the composer and summarizes both conversation and file changes.
- Prompt edits can be rolled back in place, while turn-scoped file checkpoints preview, restore, or discard changes with conflict and unavailable states reported explicitly.
- Rewind handles completed, interrupted, partial, steered, paginated, and output-free turns; the selected transcript snapshot remains stable even when an Esc interruption arrives while rollback is pending.
- Safety-buffered requests show progress and can offer one retry with a faster model by forking from the source thread.

## Model lifecycle and context

- Model metadata and context-window defaults have been refreshed, including models with context windows up to one million tokens; the status line can show usage and remaining capacity.
- Compaction is now explicit and observable, with local, remote v1, and remote v2 modes, lifecycle progress, token accounting, and model-aware fallback behavior.
- Active turns can refresh runtime settings and switch providers, request retries use bounded budgets, and request progress reports bytes sent and received.
- Environment metadata (platform, path convention, and shell flavor) is supplied to the model, and shell guidance adapts to Windows Git Bash and PowerShell.
- Code Mode admits nested tool calls safely in parallel and applies per-cell wait backoff for long-running work.
- Subagents start with fresh v2 contexts by default; parallel tools, unified-exec waits, background-terminal polling, and session shutdown checkpoints have more consistent lifecycle handling.
- Restricted commands are approved before execution, MCP execution authority can switch without reconnecting, and hook discovery is confined to `CODEX_HOME`.
- Project instructions accept `AGENTS.md` first and use `CLAUDE.md` as a built-in fallback. Unknown `-c` configuration keys are ignored so shared configuration can be reused safely.

## App Server and storage

- Threads can be reverted in place while preserving durable history and reload state.
- App Server v2 exposes provider and request lifecycle notifications, configurable compaction, thread provider updates, and file-change read/restore/discard APIs.
- A dedicated file-checkpoint store records create, update, and delete operations and reports which files are restorable, conflicting, or unavailable.
- Canonical file-change history is reconstructed correctly after reload, and generated JSON/TypeScript schemas stay aligned with the protocol.

# Special Thanks
 - [TheSmallHanCat](https://github.com/TheSmallHanCat)
 - [Cometix Codex](https://linux.do/t/topic/1481797)
 - [LINUX DO](https://linux.do/)