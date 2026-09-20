use std::io;

use codex_file_system::ExecutorFileSystem;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use tokio::sync::Semaphore;

use crate::FileAfterImage;
use crate::FileCheckpointError;
use crate::FileCheckpointStore;
use crate::FileRestoreChangeKind;
use crate::FileRestoreDisposition;
use crate::FileRestoreOutcome;
use crate::FileRestorePreview;
use crate::FileRestorePreviewEntry;
use crate::FileSystemsByEnvironment;
use crate::model::CheckpointState;
use crate::model::FileKey;
use crate::model::JournalEntry;
use crate::model::PlannedRestore;
use crate::model::RestoredIdentity;
use crate::model::change_kind;
use crate::model::identity_from_image;
use crate::model::persisted_path;
use crate::storage::current_identity;
use crate::storage::load_state;

const MAX_RETAINED_TURNS: usize = 100;
const JOURNAL_FILE: &str = "journal.jsonl";
const BLOBS_DIR: &str = "blobs";

#[derive(Clone, Debug)]
struct InspectedRestore {
    plan: PlannedRestore,
    preview: FileRestorePreviewEntry,
}

impl FileCheckpointStore {
    pub async fn open(
        codex_home: &AbsolutePathBuf,
        thread_id: ThreadId,
    ) -> Result<Self, FileCheckpointError> {
        let thread_dir = codex_home
            .join("file-checkpoints")
            .join(thread_id.to_string());
        let journal_path = thread_dir.join(JOURNAL_FILE);
        let blobs_dir = thread_dir.join(BLOBS_DIR);
        let state = match tokio::fs::read_to_string(journal_path.as_path()).await {
            Ok(contents) => load_state(&contents),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(CheckpointState::default()),
            Err(err) => Err(FileCheckpointError::Io(err)),
        }?;
        Ok(Self {
            thread_dir,
            journal_path,
            blobs_dir,
            state: std::sync::Mutex::new(state),
            operation_lock: Semaphore::new(/*permits*/ 1),
        })
    }

    pub async fn begin_turn(&self, turn_id: &str) -> Result<(), FileCheckpointError> {
        let _permit = self
            .operation_lock
            .acquire()
            .await
            .map_err(|_| FileCheckpointError::OperationLockClosed)?;
        if self
            .state()?
            .turns
            .iter()
            .any(|turn| turn.turn_id == turn_id)
        {
            return Ok(());
        }

        let begin = JournalEntry::BeginTurn {
            turn_id: turn_id.to_string(),
        };
        self.append(&begin).await?;
        self.state()?.apply(begin);

        let pruned_turn_id = {
            let state = self.state()?;
            (state.turns.len() > MAX_RETAINED_TURNS)
                .then(|| state.turns.first().map(|turn| turn.turn_id.clone()))
                .flatten()
        };
        if let Some(pruned_turn_id) = pruned_turn_id {
            let prune = JournalEntry::PruneTurn {
                turn_id: pruned_turn_id,
            };
            self.append(&prune).await?;
            self.state()?.apply(prune);
            self.remove_unreferenced_blobs().await?;
        }
        Ok(())
    }

    pub async fn capture_before_write(
        &self,
        turn_id: &str,
        environment_id: &str,
        file_system: &dyn ExecutorFileSystem,
        paths: &[PathUri],
    ) -> Result<(), FileCheckpointError> {
        let _permit = self
            .operation_lock
            .acquire()
            .await
            .map_err(|_| FileCheckpointError::OperationLockClosed)?;
        if !self
            .state()?
            .turns
            .iter()
            .any(|turn| turn.turn_id == turn_id)
        {
            let begin = JournalEntry::BeginTurn {
                turn_id: turn_id.to_string(),
            };
            self.append(&begin).await?;
            self.state()?.apply(begin);
        }

        for path in paths {
            let file = FileKey {
                environment_id: environment_id.to_string(),
                path: persisted_path(path)?,
            };
            let already_captured = self
                .state()?
                .turns
                .iter()
                .find(|turn| turn.turn_id == turn_id)
                .is_some_and(|turn| turn.before_by_file.contains_key(&file));
            if already_captured {
                continue;
            }

            let before = self
                .read_before_image(file_system, path)
                .await
                .map_err(|err| FileCheckpointError::InvalidData(format!("{path}: {err}")))?;
            let entry = JournalEntry::BeforeImage {
                turn_id: turn_id.to_string(),
                file,
                before,
            };
            self.append(&entry).await?;
            self.state()?.apply(entry);
        }
        Ok(())
    }

    pub async fn record_after_images(
        &self,
        environment_id: &str,
        images: &[FileAfterImage],
    ) -> Result<(), FileCheckpointError> {
        let _permit = self
            .operation_lock
            .acquire()
            .await
            .map_err(|_| FileCheckpointError::OperationLockClosed)?;
        for image in images {
            let entry = JournalEntry::AfterImage {
                file: FileKey {
                    environment_id: environment_id.to_string(),
                    path: persisted_path(&image.path)?,
                },
                after: identity_from_image(&image.image),
            };
            self.append(&entry).await?;
            self.state()?.apply(entry);
        }
        Ok(())
    }

    pub async fn preview(
        &self,
        before_turn_id: &str,
        file_systems: &FileSystemsByEnvironment,
    ) -> Result<FileRestorePreview, FileCheckpointError> {
        let _permit = self
            .operation_lock
            .acquire()
            .await
            .map_err(|_| FileCheckpointError::OperationLockClosed)?;
        let plan = self.state()?.restore_plan(before_turn_id);
        let inspected = self.inspect_plan(plan, file_systems).await;
        Ok(FileRestorePreview {
            files: inspected.into_iter().map(|item| item.preview).collect(),
        })
    }

    pub async fn restore(
        &self,
        before_turn_id: &str,
        file_systems: &FileSystemsByEnvironment,
    ) -> Result<FileRestoreOutcome, FileCheckpointError> {
        let _permit = self
            .operation_lock
            .acquire()
            .await
            .map_err(|_| FileCheckpointError::OperationLockClosed)?;
        let plan = self.state()?.restore_plan(before_turn_id);
        let inspected = self.inspect_plan(plan, file_systems).await;
        let mut outcome = FileRestoreOutcome {
            restored: Vec::new(),
            skipped: Vec::new(),
            failed: Vec::new(),
        };
        let mut restored_identities = Vec::new();

        for inspected in inspected {
            if inspected.preview.disposition != FileRestoreDisposition::Restorable {
                outcome.skipped.push(inspected.preview);
                continue;
            }
            let Some(file_system) = file_systems.get(&inspected.plan.file.environment_id) else {
                outcome.skipped.push(inspected.preview);
                continue;
            };
            let path = match inspected.plan.file.path.to_inferred_path_uri() {
                Some(path) => path,
                None => {
                    let mut preview = inspected.preview;
                    preview.disposition = FileRestoreDisposition::Unavailable;
                    preview.detail = Some("stored path could not be resolved".to_string());
                    outcome.failed.push(preview);
                    continue;
                }
            };
            match current_identity(file_system.as_ref(), &path).await {
                Ok(current) if inspected.plan.expected_current.as_ref() == Some(&current) => {}
                Ok(_) => {
                    let mut preview = inspected.preview;
                    preview.disposition = FileRestoreDisposition::Conflict;
                    preview.detail =
                        Some("file changed after the restore preview was prepared".to_string());
                    outcome.skipped.push(preview);
                    continue;
                }
                Err(err) => {
                    let mut preview = inspected.preview;
                    preview.disposition = FileRestoreDisposition::Unavailable;
                    preview.detail = Some(err.to_string());
                    outcome.failed.push(preview);
                    continue;
                }
            }
            match self
                .restore_one(file_system.as_ref(), &path, &inspected.plan.target)
                .await
            {
                Ok(()) => {
                    restored_identities.push(RestoredIdentity {
                        file: inspected.plan.file,
                        image: inspected.plan.target.identity(),
                    });
                    outcome.restored.push(inspected.preview);
                }
                Err(err) => {
                    let mut preview = inspected.preview;
                    preview.disposition = FileRestoreDisposition::Unavailable;
                    preview.detail = Some(err.to_string());
                    outcome.failed.push(preview);
                }
            }
        }

        if !restored_identities.is_empty() {
            let entry = JournalEntry::Restore {
                restored: restored_identities,
            };
            self.append(&entry).await?;
            self.state()?.apply(entry);
        }
        Ok(outcome)
    }

    pub async fn discard_from_turn(&self, before_turn_id: &str) -> Result<(), FileCheckpointError> {
        let _permit = self
            .operation_lock
            .acquire()
            .await
            .map_err(|_| FileCheckpointError::OperationLockClosed)?;
        if !self
            .state()?
            .turns
            .iter()
            .any(|turn| turn.turn_id == before_turn_id)
        {
            return Ok(());
        }
        let entry = JournalEntry::DiscardFromTurn {
            turn_id: before_turn_id.to_string(),
        };
        self.append(&entry).await?;
        self.state()?.apply(entry);
        self.remove_unreferenced_blobs().await
    }

    async fn inspect_plan(
        &self,
        plan: Vec<PlannedRestore>,
        file_systems: &FileSystemsByEnvironment,
    ) -> Vec<InspectedRestore> {
        let mut inspected = Vec::with_capacity(plan.len());
        for mut plan in plan {
            let mut preview = FileRestorePreviewEntry {
                environment_id: plan.file.environment_id.clone(),
                path: plan.file.path.clone(),
                change_kind: FileRestoreChangeKind::Update,
                disposition: FileRestoreDisposition::Unavailable,
                detail: None,
            };
            let Some(file_system) = file_systems.get(&plan.file.environment_id) else {
                preview.detail = Some("environment is unavailable".to_string());
                inspected.push(InspectedRestore { plan, preview });
                continue;
            };
            let Some(path) = plan.file.path.to_inferred_path_uri() else {
                preview.detail = Some("stored path could not be resolved".to_string());
                inspected.push(InspectedRestore { plan, preview });
                continue;
            };
            match current_identity(file_system.as_ref(), &path).await {
                Ok(current) if current == plan.target.identity() => continue,
                Ok(current) => {
                    preview.change_kind = change_kind(&current, &plan.target);
                    // An explicit rewind restores the durable before-image, including when the
                    // tracked file was edited again outside the tool. Still detect writes racing
                    // this restore's inspection before replacing the file.
                    plan.expected_current = Some(current);
                    preview.disposition = FileRestoreDisposition::Restorable;
                }
                Err(err) => {
                    preview.detail = Some(err.to_string());
                }
            }
            inspected.push(InspectedRestore { plan, preview });
        }
        inspected
    }
}
