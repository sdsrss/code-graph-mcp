use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;
use anyhow::Result;
use ignore::WalkBuilder;

use crate::utils::config::detect_language;

pub struct DiffResult {
    pub new_files: Vec<String>,
    pub changed_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

/// Hash a file using streaming blake3 (constant memory, handles large files).
pub fn hash_file(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 16384];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn scan_directory(root: &Path) -> Result<HashMap<String, String>> {
    let mut hashes = HashMap::new();
    let walker = WalkBuilder::new(root)
        .hidden(true)       // skip hidden files
        .git_ignore(true)   // respect .gitignore
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Ok(rel) = path.strip_prefix(root) {
            let rel_str = rel.to_string_lossy().to_string();
            // Skip .git files and .gitignore
            if rel_str == ".git" || rel_str.starts_with(".git/") || rel_str.starts_with(".git\\") {
                continue;
            }
            // Only hash files with supported language extensions
            if detect_language(&rel_str).is_none() {
                continue;
            }
            let hash = match hash_file(path) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("Skipping file (hash error): {}: {}", path.display(), e);
                    continue;
                }
            };
            hashes.insert(rel_str, hash);
        }
    }
    Ok(hashes)
}

/// Cache of directory and file modification times for skipping unchanged subtrees.
#[derive(Debug, Clone, Default)]
pub struct DirectoryCache {
    dir_mtimes: HashMap<String, SystemTime>,
    /// Per-file mtime cache. Used to detect content modifications in directories
    /// whose own mtime hasn't changed (dir mtime only changes on file add/remove,
    /// not on content modification in ext4/btrfs).
    file_mtimes: HashMap<String, SystemTime>,
}

/// Scan directory with optional mtime cache. Directories whose mtime
/// hasn't changed since the cached value can skip file hashing.
pub fn scan_directory_cached(
    root: &Path,
    cache: Option<&DirectoryCache>,
) -> Result<(HashMap<String, String>, DirectoryCache)> {
    let mut hashes = HashMap::new();
    let mut new_cache = DirectoryCache::default();
    let mut changed_dirs: HashSet<String> = HashSet::new();

    // Collect all entries
    let entries: Vec<_> = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .filter_map(|e| e.ok())
        .collect();

    // Pass 1: identify changed directories
    for entry in &entries {
        if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            let rel_str = rel.to_string_lossy().to_string();
            if rel_str.starts_with(".git") { continue; }

            if let Ok(meta) = entry.path().metadata() {
                if let Ok(mtime) = meta.modified() {
                    new_cache.dir_mtimes.insert(rel_str.clone(), mtime);
                    let is_changed = match cache {
                        Some(c) => c.dir_mtimes.get(&rel_str) != Some(&mtime),
                        None => true,
                    };
                    if is_changed {
                        changed_dirs.insert(rel_str);
                    }
                }
            }
        }
    }
    // Root always considered changed
    changed_dirs.insert(String::new());

    // Pass 2: hash files in changed directories, and check file mtime in unchanged directories.
    // Directory mtime only changes on file add/remove (not content edits on ext4/btrfs),
    // so we also check individual file mtimes to catch content modifications.
    for entry in &entries {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Ok(rel) = path.strip_prefix(root) {
            let rel_str = rel.to_string_lossy().to_string();
            if rel_str == ".git" || rel_str.starts_with(".git/") || rel_str.starts_with(".git\\") {
                continue;
            }
            if detect_language(&rel_str).is_none() {
                continue;
            }

            let parent_dir = rel.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            // Track file mtime in the new cache
            let file_mtime = path.metadata().ok().and_then(|m| m.modified().ok());
            if let Some(mtime) = file_mtime {
                new_cache.file_mtimes.insert(rel_str.clone(), mtime);
            }

            if !changed_dirs.contains(&parent_dir) {
                // Directory unchanged — check if individual file mtime changed
                let file_changed = match (file_mtime, cache.and_then(|c| c.file_mtimes.get(&rel_str))) {
                    (Some(current), Some(cached)) => current != *cached,
                    (Some(_), None) => true, // No cached mtime — treat as changed
                    _ => false,
                };
                if !file_changed {
                    continue;
                }
            }

            let hash = match hash_file(path) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("Skipping file (hash error): {}: {}", path.display(), e);
                    continue;
                }
            };
            hashes.insert(rel_str, hash);
        }
    }

    Ok((hashes, new_cache))
}

pub fn compute_diff(
    old: &HashMap<String, String>,
    current: &HashMap<String, String>,
) -> DiffResult {
    let mut new_files = Vec::new();
    let mut changed_files = Vec::new();
    let mut deleted_files = Vec::new();

    for (path, hash) in current {
        match old.get(path) {
            None => new_files.push(path.clone()),
            Some(old_hash) if old_hash != hash => changed_files.push(path.clone()),
            _ => {}
        }
    }

    for path in old.keys() {
        if !current.contains_key(path) {
            deleted_files.push(path.clone());
        }
    }

    DiffResult {
        new_files,
        changed_files,
        deleted_files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_hash_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();
        let hash = hash_file(&file).unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // blake3 hex = 64 chars
    }

    #[test]
    fn test_diff_detects_new_files() {
        let old: HashMap<String, String> = HashMap::new();
        let mut current = HashMap::new();
        current.insert("a.rs".into(), "hash1".into());

        let diff = compute_diff(&old, &current);
        assert_eq!(diff.new_files.len(), 1);
        assert_eq!(diff.changed_files.len(), 0);
        assert_eq!(diff.deleted_files.len(), 0);
    }

    #[test]
    fn test_diff_detects_changed_files() {
        let mut old = HashMap::new();
        old.insert("a.rs".into(), "hash1".into());

        let mut current = HashMap::new();
        current.insert("a.rs".into(), "hash2".into());

        let diff = compute_diff(&old, &current);
        assert_eq!(diff.new_files.len(), 0);
        assert_eq!(diff.changed_files.len(), 1);
        assert_eq!(diff.deleted_files.len(), 0);
    }

    #[test]
    fn test_diff_detects_deleted_files() {
        let mut old = HashMap::new();
        old.insert("a.rs".into(), "hash1".into());
        let current: HashMap<String, String> = HashMap::new();

        let diff = compute_diff(&old, &current);
        assert_eq!(diff.deleted_files.len(), 1);
    }

    #[test]
    fn test_scan_directory_cached_skips_unchanged() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main(){}").unwrap();

        let (hashes1, cache1) = scan_directory_cached(tmp.path(), None).unwrap();
        assert_eq!(hashes1.len(), 1);

        // Second scan with same cache: should return empty (dirs unchanged → files skipped)
        let (hashes2, _cache2) = scan_directory_cached(tmp.path(), Some(&cache1)).unwrap();
        // Files in unchanged dirs are skipped
        assert_eq!(hashes2.len(), 0);
    }

    #[test]
    fn test_scan_directory_cached_detects_new_file() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main(){}").unwrap();

        let (_hashes1, cache1) = scan_directory_cached(tmp.path(), None).unwrap();

        // Add a new file (changes directory mtime)
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(tmp.path().join("src/lib.rs"), "pub fn lib(){}").unwrap();

        let (hashes2, _cache2) = scan_directory_cached(tmp.path(), Some(&cache1)).unwrap();
        // Both files should be hashed since src/ dir changed
        assert_eq!(hashes2.len(), 2);
        assert!(hashes2.contains_key("src/lib.rs"));
    }

    #[test]
    fn test_scan_directory_respects_gitignore() {
        let tmp = TempDir::new().unwrap();
        // Initialize a git repo so that .gitignore rules are respected by the ignore crate
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".gitignore"), "node_modules/\n*.log").unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main(){}").unwrap();
        fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        fs::write(tmp.path().join("node_modules/pkg.js"), "x").unwrap();
        fs::write(tmp.path().join("debug.log"), "log").unwrap();

        let hashes = scan_directory(tmp.path()).unwrap();
        assert!(hashes.contains_key("src/main.rs"));
        assert!(!hashes.contains_key("node_modules/pkg.js"));
        assert!(!hashes.contains_key("debug.log"));
    }
}
