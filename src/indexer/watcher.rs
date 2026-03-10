use anyhow::Result;
use notify::{Watcher, RecursiveMode, Event};
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
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let paths: Vec<String> = event.paths.iter()
                    .filter_map(|p| p.to_str().map(String::from))
                    .collect();
                if !paths.is_empty() {
                    let _ = tx.send(WatchEvent::Changed(paths));
                }
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }

    pub fn stop(self) {
        drop(self._watcher);
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

        watcher.stop();
    }
}
