use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::request_processors::ConfigRequestProcessor;
use codex_config::CONFIG_TOML_FILE;
use codex_config::ConfigLayerSource;
use codex_core::config::Config;
use codex_file_watcher::FileWatcher;
use codex_file_watcher::FileWatcherSubscriber;
use codex_file_watcher::Receiver;
use codex_file_watcher::ThrottledWatchReceiver;
use codex_file_watcher::WatchPath;
use codex_file_watcher::WatchRegistration;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::DropGuard;
use tracing::warn;

const WATCHER_THROTTLE_INTERVAL: Duration = Duration::from_millis(500);

/// Reloads user configuration after a watched config layer changes on disk.
pub(crate) struct ConfigWatcher {
    _subscriber: FileWatcherSubscriber,
    registrations: Mutex<Vec<WatchRegistration>>,
    shutdown_token: CancellationToken,
    _shutdown_drop_guard: DropGuard,
}

impl ConfigWatcher {
    pub(crate) fn new(config: &Config, config_processor: ConfigRequestProcessor) -> Arc<Self> {
        let file_watcher = match FileWatcher::new() {
            Ok(file_watcher) => Arc::new(file_watcher),
            Err(err) => {
                warn!("failed to initialize config file watcher: {err}");
                Arc::new(FileWatcher::noop())
            }
        };
        let (subscriber, rx) = file_watcher.add_subscriber();
        let registration = subscriber.register_paths(
            config_paths(config)
                .into_iter()
                .map(|path| WatchPath {
                    path,
                    recursive: false,
                })
                .collect(),
        );
        let shutdown_token = CancellationToken::new();
        let shutdown_drop_guard = shutdown_token.clone().drop_guard();
        Self::spawn_event_loop(rx, config_processor, shutdown_token.child_token());
        Arc::new(Self {
            _subscriber: subscriber,
            registrations: Mutex::new(vec![registration]),
            shutdown_token,
            _shutdown_drop_guard: shutdown_drop_guard,
        })
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown_token.cancel();
        self.registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn spawn_event_loop(
        rx: Receiver,
        config_processor: ConfigRequestProcessor,
        shutdown_token: CancellationToken,
    ) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("config watcher listener skipped: no Tokio runtime available");
            return;
        };
        handle.spawn(async move {
            let mut rx = ThrottledWatchReceiver::new(rx, WATCHER_THROTTLE_INTERVAL);
            loop {
                let event = tokio::select! {
                    _ = shutdown_token.cancelled() => break,
                    event = rx.recv() => event,
                };
                if event.is_none() {
                    break;
                }
                config_processor.handle_config_mutation().await;
                config_processor.reload_user_config().await;
            }
        });
    }
}

fn config_paths(config: &Config) -> Vec<std::path::PathBuf> {
    let mut paths = config
        .config_layer_stack
        .all_layers_low_to_high()
        .filter_map(|layer| match &layer.name {
            ConfigLayerSource::User { file, .. } => Some(file.clone().into_path_buf()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if paths.is_empty() {
        paths.push(config.codex_home.join(CONFIG_TOML_FILE).into_path_buf());
    }
    paths.sort();
    paths.dedup();
    paths
}
