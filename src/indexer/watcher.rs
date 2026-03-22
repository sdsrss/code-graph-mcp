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

pub struct FileWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl FileWatcher {
    pub fn start(root: &Path, tx: mpsc::Sender<WatchEvent>) -> Result<Self> {
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
                                Ok(rel) => rel.to_str().map(String::from),
                                Err(_) => {
                                    tracing::debug!("watcher: dropping out-of-root path {:?}", p);
                                    None
                                }
                            }
                        })
                        .collect();
                    if !paths.is_empty() {
                        if let Err(e) = tx.send(WatchEvent::Changed(paths)) {
                            tracing::debug!("Watcher channel send failed (receiver dropped): {}", e);
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
    use std::{fs, time::Duration, thread};

    #[test]
    fn test_watcher_detects_file_changes() {
        let tmp = TempDir::new().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = FileWatcher::start(tmp.path(), tx).unwrap();

        // Create a file
        fs::write(tmp.path().join("test.ts"), "function foo() {}").unwrap();
        thread::sleep(Duration::from_millis(200));

        let events: Vec<WatchEvent> = rx.try_iter().collect();
        assert!(!events.is_empty());

        drop(watcher);
    }
}
