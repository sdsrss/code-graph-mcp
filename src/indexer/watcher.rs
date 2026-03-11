use anyhow::Result;
use notify::{Watcher, RecursiveMode, Event, EventKind};
use std::path::Path;
use std::sync::mpsc;

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
                    // Only react to content-modifying events, not metadata/access
                    if !matches!(
                        event.kind,
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                    ) {
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
                        let _ = tx.send(WatchEvent::Changed(paths));
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
