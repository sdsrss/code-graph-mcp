use anyhow::Result;
use notify::{Watcher, RecursiveMode, Event, EventKind};
use notify::event::ModifyKind;
use std::path::Path;
use std::sync::mpsc;

/// Check if a file system event kind represents a content change (not just metadata/access).
fn is_content_event(kind: &EventKind) -> bool {
    matches!(kind,
        EventKind::Create(_)
        | EventKind::Remove(_)
        | EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Name(_))
        | EventKind::Modify(ModifyKind::Any) // catch-all for platforms that don't distinguish
    )
}

pub enum WatchEvent {
    Changed(Vec<String>),
}

/// Bound on pending watcher events. A full channel means the main-loop consumer
/// is lagging; subsequent events are dropped (logged). This is safe because the
/// downstream merkle rescan is idempotent — as long as *any* event remains in
/// the buffer when drain_watcher_events runs, the bool signal fires and the
/// rescan picks up all on-disk changes regardless of dropped events.
pub const WATCHER_CHANNEL_BOUND: usize = 4096;

pub struct FileWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl FileWatcher {
    pub fn start(root: &Path, tx: mpsc::SyncSender<WatchEvent>) -> Result<Self> {
        // macOS FSEvents reports event paths via realpath, so a watch on
        // `/var/folders/xx/T/foo` against a non-canonical `root` drops
        // every event at `strip_prefix` below (the symlink target
        // `/private/var/...` never has `/var/folders/...` as a prefix).
        // We canonicalize on Unix to fix this; on Windows we must NOT
        // canonicalize, because `std::fs::canonicalize` there returns
        // UNC paths (`\\?\C:\...`) while notify's ReadDirectoryChangesW
        // backend emits plain `C:\...` paths — which would re-break
        // `strip_prefix` in the other direction.
        #[cfg(not(windows))]
        let root_path = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        #[cfg(windows)]
        let root_path = root.to_path_buf();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Only react to content-modifying events.
                    // Skip metadata-only changes (chmod, xattr) and access events
                    // which would trigger unnecessary incremental index scans.
                    if !is_content_event(&event.kind) {
                        return;
                    }
                    // Convert to relative paths for consistency with the indexer pipeline.
                    // Skip paths that fail strip_prefix (absolute paths never match DB relative paths).
                    let paths: Vec<String> = event.paths.iter()
                        .filter_map(|p| {
                            match p.strip_prefix(&root_path) {
                                // Normalize `\` → `/` on Windows so the downstream
                                // pipeline and DB consumers see the same path
                                // shape they see on Unix (see merkle::normalize_rel_path).
                                Ok(rel) => Some(crate::indexer::merkle::normalize_rel_path(rel)),
                                Err(_) => {
                                    tracing::debug!("watcher: dropping out-of-root path {:?}", p);
                                    None
                                }
                            }
                        })
                        .collect();
                    if !paths.is_empty() {
                        match tx.try_send(WatchEvent::Changed(paths)) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(_)) => {
                                tracing::warn!(
                                    "Watcher channel full ({} events buffered); dropping event. \
                                     Main loop is lagging — next merkle rescan will pick up all changes.",
                                    WATCHER_CHANNEL_BOUND
                                );
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                tracing::debug!("Watcher channel receiver dropped");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("File watcher error: {}", e);
                }
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::{fs, time::Duration};

    #[test]
    fn test_watcher_detects_file_changes() {
        let tmp = TempDir::new().unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(WATCHER_CHANNEL_BOUND);
        // FileWatcher::start canonicalizes internally — FSEvents on macOS
        // reports paths via realpath, so a non-canonical root would drop
        // every event at strip_prefix.
        let watcher = FileWatcher::start(tmp.path(), tx).unwrap();

        // Create a file
        fs::write(tmp.path().join("test.ts"), "function foo() {}").unwrap();

        // Wait for at least one event. macOS FSEvents (and loaded CI runners
        // in general) can take several seconds to coalesce and emit; 5s was
        // empirically flaky on GH macOS runners, 15s is not.
        let first = rx.recv_timeout(Duration::from_secs(15))
            .expect("timed out waiting for watcher event");
        let mut events = vec![first];
        // Drain any additional buffered events
        events.extend(rx.try_iter());
        assert!(!events.is_empty());

        drop(watcher);
    }
}
