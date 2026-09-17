use std::collections::HashMap;
use std::sync::Arc;

use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::LOCAL_FS;
use codex_file_system::WriteFileOptions;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::FileAfterImage;
use super::FileCheckpointStore;
use super::FileImage;
use super::FileRestoreChangeKind;
use super::FileRestoreDisposition;
use super::FileSystemsByEnvironment;

const ENVIRONMENT_ID: &str = "local";

fn absolute(path: &std::path::Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute path")
}

fn path_uri(path: &std::path::Path) -> PathUri {
    PathUri::from_host_native_path(path).expect("path URI")
}

fn file_systems() -> FileSystemsByEnvironment {
    HashMap::from([(
        ENVIRONMENT_ID.to_string(),
        Arc::clone(&LOCAL_FS) as Arc<dyn ExecutorFileSystem>,
    )])
}

async fn write(path: &PathUri, contents: &str) {
    LOCAL_FS
        .write_file(
            path,
            contents.as_bytes().to_vec(),
            WriteFileOptions {
                follow_symlinks: false,
            },
            /*sandbox*/ None,
        )
        .await
        .expect("write file");
}

#[tokio::test]
async fn restores_file_to_state_before_selected_turn() {
    let home = TempDir::new().expect("temp dir");
    let workspace = TempDir::new().expect("temp dir");
    let path = path_uri(&workspace.path().join("sample.txt"));
    write(&path, "original\n").await;
    let store = FileCheckpointStore::open(&absolute(home.path()), ThreadId::new())
        .await
        .expect("open checkpoint store");

    store.begin_turn("turn-1").await.expect("begin turn");
    store
        .capture_before_write(
            "turn-1",
            ENVIRONMENT_ID,
            LOCAL_FS.as_ref(),
            std::slice::from_ref(&path),
        )
        .await
        .expect("capture turn one");
    write(&path, "one\n").await;
    store
        .record_after_images(
            ENVIRONMENT_ID,
            &[FileAfterImage {
                path: path.clone(),
                image: FileImage::Contents(b"one\n".to_vec()),
            }],
        )
        .await
        .expect("record turn one");

    store.begin_turn("turn-2").await.expect("begin turn");
    store
        .capture_before_write(
            "turn-2",
            ENVIRONMENT_ID,
            LOCAL_FS.as_ref(),
            std::slice::from_ref(&path),
        )
        .await
        .expect("capture turn two");
    write(&path, "two\n").await;
    store
        .record_after_images(
            ENVIRONMENT_ID,
            &[FileAfterImage {
                path: path.clone(),
                image: FileImage::Contents(b"two\n".to_vec()),
            }],
        )
        .await
        .expect("record turn two");

    let preview = store
        .preview("turn-1", &file_systems())
        .await
        .expect("preview restore");
    assert_eq!(preview.files.len(), 1);
    assert_eq!(preview.files[0].change_kind, FileRestoreChangeKind::Update);
    assert_eq!(
        preview.files[0].disposition,
        FileRestoreDisposition::Restorable
    );

    let outcome = store
        .restore("turn-1", &file_systems())
        .await
        .expect("restore files");
    assert_eq!(outcome.restored, preview.files);
    assert_eq!(
        std::fs::read_to_string(path.to_path_buf()).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn restores_created_file_by_deleting_it() {
    let home = TempDir::new().expect("temp dir");
    let workspace = TempDir::new().expect("temp dir");
    let path = path_uri(&workspace.path().join("created.txt"));
    let store = FileCheckpointStore::open(&absolute(home.path()), ThreadId::new())
        .await
        .expect("open checkpoint store");
    store.begin_turn("turn-1").await.expect("begin turn");
    store
        .capture_before_write(
            "turn-1",
            ENVIRONMENT_ID,
            LOCAL_FS.as_ref(),
            std::slice::from_ref(&path),
        )
        .await
        .expect("capture absent file");
    write(&path, "created\n").await;
    store
        .record_after_images(
            ENVIRONMENT_ID,
            &[FileAfterImage {
                path: path.clone(),
                image: FileImage::Contents(b"created\n".to_vec()),
            }],
        )
        .await
        .expect("record created file");

    let outcome = store
        .restore("turn-1", &file_systems())
        .await
        .expect("restore files");
    assert_eq!(outcome.restored.len(), 1);
    assert_eq!(
        outcome.restored[0].change_kind,
        FileRestoreChangeKind::Delete
    );
    assert!(!path.to_path_buf().exists());
}

#[tokio::test]
async fn skips_file_changed_outside_managed_tools() {
    let home = TempDir::new().expect("temp dir");
    let workspace = TempDir::new().expect("temp dir");
    let path = path_uri(&workspace.path().join("sample.txt"));
    write(&path, "original\n").await;
    let store = FileCheckpointStore::open(&absolute(home.path()), ThreadId::new())
        .await
        .expect("open checkpoint store");
    store.begin_turn("turn-1").await.expect("begin turn");
    store
        .capture_before_write(
            "turn-1",
            ENVIRONMENT_ID,
            LOCAL_FS.as_ref(),
            std::slice::from_ref(&path),
        )
        .await
        .expect("capture file");
    write(&path, "managed\n").await;
    store
        .record_after_images(
            ENVIRONMENT_ID,
            &[FileAfterImage {
                path: path.clone(),
                image: FileImage::Contents(b"managed\n".to_vec()),
            }],
        )
        .await
        .expect("record managed file");
    write(&path, "manual\n").await;

    let outcome = store
        .restore("turn-1", &file_systems())
        .await
        .expect("restore files");
    assert_eq!(outcome.restored, Vec::new());
    assert_eq!(outcome.skipped.len(), 1);
    assert_eq!(
        outcome.skipped[0].disposition,
        FileRestoreDisposition::Conflict
    );
    assert_eq!(
        std::fs::read_to_string(path.to_path_buf()).unwrap(),
        "manual\n"
    );
}

#[tokio::test]
async fn ignores_captured_file_that_managed_tools_did_not_change() {
    let home = TempDir::new().expect("temp dir");
    let workspace = TempDir::new().expect("temp dir");
    let changed_path = path_uri(&workspace.path().join("changed.txt"));
    let unchanged_path = path_uri(&workspace.path().join("unchanged.txt"));
    write(&changed_path, "original changed\n").await;
    write(&unchanged_path, "original unchanged\n").await;
    let store = FileCheckpointStore::open(&absolute(home.path()), ThreadId::new())
        .await
        .expect("open checkpoint store");

    store.begin_turn("turn-1").await.expect("begin turn");
    store
        .capture_before_write(
            "turn-1",
            ENVIRONMENT_ID,
            LOCAL_FS.as_ref(),
            &[changed_path.clone(), unchanged_path.clone()],
        )
        .await
        .expect("capture candidate files");
    write(&changed_path, "managed\n").await;
    store
        .record_after_images(
            ENVIRONMENT_ID,
            &[FileAfterImage {
                path: changed_path.clone(),
                image: FileImage::Contents(b"managed\n".to_vec()),
            }],
        )
        .await
        .expect("record changed file");
    write(&unchanged_path, "manual\n").await;

    let preview = store
        .preview("turn-1", &file_systems())
        .await
        .expect("preview restore");
    assert_eq!(
        preview.files,
        vec![super::FileRestorePreviewEntry {
            environment_id: ENVIRONMENT_ID.to_string(),
            path: super::model::persisted_path(&changed_path).expect("persisted path"),
            change_kind: FileRestoreChangeKind::Update,
            disposition: FileRestoreDisposition::Restorable,
            detail: None,
        }]
    );
}

#[tokio::test]
async fn reloads_journal_and_discards_rewound_turns() {
    let home = TempDir::new().expect("temp dir");
    let workspace = TempDir::new().expect("temp dir");
    let thread_id = ThreadId::new();
    let path = path_uri(&workspace.path().join("sample.txt"));
    write(&path, "original\n").await;
    let store = FileCheckpointStore::open(&absolute(home.path()), thread_id)
        .await
        .expect("open checkpoint store");
    store.begin_turn("turn-1").await.expect("begin turn");
    store
        .capture_before_write(
            "turn-1",
            ENVIRONMENT_ID,
            LOCAL_FS.as_ref(),
            std::slice::from_ref(&path),
        )
        .await
        .expect("capture file");
    write(&path, "managed\n").await;
    store
        .record_after_images(
            ENVIRONMENT_ID,
            &[FileAfterImage {
                path: path.clone(),
                image: FileImage::Contents(b"managed\n".to_vec()),
            }],
        )
        .await
        .expect("record file");
    drop(store);

    let store = FileCheckpointStore::open(&absolute(home.path()), thread_id)
        .await
        .expect("reopen checkpoint store");
    assert_eq!(
        store
            .preview("turn-1", &file_systems())
            .await
            .expect("preview restored journal")
            .files
            .len(),
        1
    );
    store
        .discard_from_turn("turn-1")
        .await
        .expect("discard turn");
    assert_eq!(
        store
            .preview("turn-1", &file_systems())
            .await
            .expect("preview discarded turn")
            .files,
        Vec::new()
    );
}
