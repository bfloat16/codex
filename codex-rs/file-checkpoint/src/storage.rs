use std::collections::HashSet;
use std::io;

use codex_file_system::CreateDirectoryOptions;
use codex_file_system::ExecutorFileSystem;
use codex_file_system::GetMetadataOptions;
use codex_file_system::ReadFileOptions;
use codex_file_system::RemoveOptions;
use codex_file_system::WriteFileOptions;
use codex_utils_path_uri::PathUri;
use tokio::io::AsyncWriteExt;

use crate::FileCheckpointError;
use crate::FileCheckpointStore;
use crate::FileImage;
use crate::model::BeforeImage;
use crate::model::CheckpointState;
use crate::model::ImageIdentity;
use crate::model::JournalEntry;
use crate::model::identity_from_image;
use crate::model::sha256;
use crate::model::validate_sha256;

impl FileCheckpointStore {
    pub(super) async fn read_before_image(
        &self,
        file_system: &dyn ExecutorFileSystem,
        path: &PathUri,
    ) -> Result<BeforeImage, FileCheckpointError> {
        match file_system
            .get_metadata(
                path,
                GetMetadataOptions {
                    follow_symlinks: false,
                },
                /*sandbox*/ None,
            )
            .await
        {
            Ok(metadata) if metadata.is_symlink => Err(FileCheckpointError::InvalidData(format!(
                "refusing to checkpoint symbolic link {path}"
            ))),
            Ok(metadata) if !metadata.is_file => Err(FileCheckpointError::InvalidData(format!(
                "refusing to checkpoint non-file path {path}"
            ))),
            Ok(_) => {
                let contents = file_system
                    .read_file(
                        path,
                        ReadFileOptions {
                            follow_symlinks: false,
                        },
                        /*sandbox*/ None,
                    )
                    .await?;
                let sha256 = sha256(&contents);
                self.write_blob(&sha256, &contents).await?;
                Ok(BeforeImage::Blob {
                    sha256,
                    size: u64::try_from(contents.len()).map_err(|_| {
                        FileCheckpointError::InvalidData("file size exceeds u64".to_string())
                    })?,
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(BeforeImage::Absent),
            Err(err) => Err(FileCheckpointError::Io(err)),
        }
    }

    pub(super) async fn restore_one(
        &self,
        file_system: &dyn ExecutorFileSystem,
        path: &PathUri,
        target: &BeforeImage,
    ) -> Result<(), FileCheckpointError> {
        match target {
            BeforeImage::Absent => file_system
                .remove(
                    path,
                    RemoveOptions {
                        recursive: false,
                        force: true,
                        follow_symlinks: false,
                    },
                    /*sandbox*/ None,
                )
                .await
                .map_err(FileCheckpointError::Io),
            BeforeImage::Blob { sha256, size } => {
                let contents = self.read_blob(sha256, *size).await?;
                if let Some(parent) = path.parent() {
                    file_system
                        .create_directory(
                            &parent,
                            CreateDirectoryOptions {
                                recursive: true,
                                follow_symlinks: false,
                            },
                            /*sandbox*/ None,
                        )
                        .await?;
                }
                file_system
                    .write_file(
                        path,
                        contents,
                        WriteFileOptions {
                            follow_symlinks: false,
                        },
                        /*sandbox*/ None,
                    )
                    .await?;
                Ok(())
            }
        }
    }

    async fn write_blob(&self, sha256: &str, contents: &[u8]) -> Result<(), FileCheckpointError> {
        validate_sha256(sha256)?;
        create_private_dir(self.blobs_dir.as_path()).await?;
        let destination = self.blobs_dir.join(sha256);
        if tokio::fs::try_exists(destination.as_path()).await? {
            return Ok(());
        }
        let temporary = self
            .blobs_dir
            .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let mut options = tokio::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(temporary.as_path()).await?;
        file.write_all(contents).await?;
        file.sync_data().await?;
        drop(file);
        match tokio::fs::rename(temporary.as_path(), destination.as_path()).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let _ = tokio::fs::remove_file(temporary.as_path()).await;
                Ok(())
            }
            Err(err) => {
                let _ = tokio::fs::remove_file(temporary.as_path()).await;
                Err(FileCheckpointError::Io(err))
            }
        }
    }

    async fn read_blob(&self, sha256: &str, size: u64) -> Result<Vec<u8>, FileCheckpointError> {
        validate_sha256(sha256)?;
        let contents = tokio::fs::read(self.blobs_dir.join(sha256).as_path()).await?;
        let actual_size = u64::try_from(contents.len()).map_err(|_| {
            FileCheckpointError::InvalidData("checkpoint blob size exceeds u64".to_string())
        })?;
        if actual_size != size || crate::model::sha256(&contents) != sha256 {
            return Err(FileCheckpointError::InvalidData(format!(
                "checkpoint blob {sha256} failed integrity validation"
            )));
        }
        Ok(contents)
    }

    pub(super) async fn append(&self, entry: &JournalEntry) -> Result<(), FileCheckpointError> {
        create_private_dir(self.thread_dir.as_path()).await?;
        let mut encoded = serde_json::to_vec(entry)
            .map_err(|err| FileCheckpointError::InvalidData(err.to_string()))?;
        encoded.push(b'\n');
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(self.journal_path.as_path()).await?;
        file.write_all(&encoded).await?;
        file.sync_data().await?;
        Ok(())
    }

    pub(super) async fn remove_unreferenced_blobs(&self) -> Result<(), FileCheckpointError> {
        let referenced = self
            .state()?
            .turns
            .iter()
            .flat_map(|turn| turn.before_by_file.values())
            .filter_map(|before| match before {
                BeforeImage::Absent => None,
                BeforeImage::Blob { sha256, .. } => Some(sha256.clone()),
            })
            .collect::<HashSet<_>>();
        let mut entries = match tokio::fs::read_dir(self.blobs_dir.as_path()).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(FileCheckpointError::Io(err)),
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if validate_sha256(&name).is_ok() && !referenced.contains(&name) {
                tokio::fs::remove_file(entry.path()).await?;
            }
        }
        Ok(())
    }

    pub(super) fn state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, CheckpointState>, FileCheckpointError> {
        self.state
            .lock()
            .map_err(|_| FileCheckpointError::LockPoisoned)
    }
}

pub(super) fn load_state(contents: &str) -> Result<CheckpointState, FileCheckpointError> {
    let mut state = CheckpointState::default();
    let lines = contents.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(_) if index + 1 == lines.len() && !contents.ends_with('\n') => break,
            Err(err) => {
                return Err(FileCheckpointError::InvalidData(format!(
                    "journal line {}: {err}",
                    index + 1
                )));
            }
        };
        state.apply(entry);
    }
    Ok(state)
}

pub(super) async fn current_identity(
    file_system: &dyn ExecutorFileSystem,
    path: &PathUri,
) -> Result<ImageIdentity, FileCheckpointError> {
    match file_system
        .get_metadata(
            path,
            GetMetadataOptions {
                follow_symlinks: false,
            },
            /*sandbox*/ None,
        )
        .await
    {
        Ok(metadata) if metadata.is_symlink => Err(FileCheckpointError::InvalidData(format!(
            "refusing to restore symbolic link {path}"
        ))),
        Ok(metadata) if !metadata.is_file => Err(FileCheckpointError::InvalidData(format!(
            "refusing to restore non-file path {path}"
        ))),
        Ok(_) => {
            let contents = file_system
                .read_file(
                    path,
                    ReadFileOptions {
                        follow_symlinks: false,
                    },
                    /*sandbox*/ None,
                )
                .await?;
            Ok(identity_from_image(&FileImage::Contents(contents)))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(ImageIdentity::Absent),
        Err(err) => Err(FileCheckpointError::Io(err)),
    }
}

async fn create_private_dir(path: &std::path::Path) -> Result<(), FileCheckpointError> {
    tokio::fs::create_dir_all(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}
