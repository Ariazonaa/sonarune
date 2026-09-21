use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub const DEFAULT_LIMIT_MB: u64 = 256;
const MIN_LIMIT_MB: u64 = 32;
const MAX_LIMIT_MB: u64 = 2_048;
const MIB: u64 = 1024 * 1024;

/// True when `s` is safe to paste into a cover-cache file name.
///
/// Item ids and image tags come verbatim out of the server's JSON. Windows
/// `Path::join` is destructive — an absolute argument (`C:\…`, `\\host\share`)
/// replaces the base entirely, and `..` segments are not normalized away — so
/// an unchecked component lets the server steer both reads and writes out of
/// the cache directory. Jellyfin sends GUIDs and hex hashes, so anything
/// outside this alphabet is not a value we need to support.
pub fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Cover cache file name for an item/tag/size, or `None` when the server-sent
/// components are not safe to use as a path component.
pub fn cover_file_name(item_id: &str, tag: &str, size: u32) -> Option<String> {
    (is_safe_path_component(item_id) && is_safe_path_component(tag))
        .then(|| format!("{item_id}-{tag}-{size}.img"))
}

/// Uniquifies temp file names so two concurrent writers of the same file (the
/// protocol handler and the player, or two requests for one cover) never
/// share a temp file before the rename. Combined with the process id, so a
/// second process writing into the same cache cannot collide either.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` so no reader ever sees a partial file: write a
/// unique temp file next to it, then rename it into place (replacing an
/// existing file). The temp file is removed on failure.
pub fn write_file_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}-{seq}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    pub size_bytes: u64,
    pub file_count: u64,
    pub limit_mb: u64,
}

#[derive(Clone)]
pub struct CoverCache {
    inner: Arc<CoverCacheInner>,
}

struct CoverCacheInner {
    dir: PathBuf,
    max_bytes: AtomicU64,
    eviction_running: AtomicBool,
    eviction_pending: AtomicBool,
}

impl CoverCache {
    pub fn new(dir: PathBuf, limit_mb: u64) -> AppResult<Self> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Other(format!("cannot create cover cache: {e}")))?;
        Ok(Self {
            inner: Arc::new(CoverCacheInner {
                dir,
                max_bytes: AtomicU64::new(normalize_limit_mb(limit_mb) * MIB),
                eviction_running: AtomicBool::new(false),
                eviction_pending: AtomicBool::new(false),
            }),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    pub fn info(&self) -> AppResult<CacheInfo> {
        let (size_bytes, file_count) = scan_cache(&self.inner.dir)
            .map_err(|e| AppError::Other(format!("cannot inspect cover cache: {e}")))?;
        Ok(CacheInfo {
            size_bytes,
            file_count,
            limit_mb: self.inner.max_bytes.load(Ordering::Relaxed) / MIB,
        })
    }

    pub fn set_limit_mb(&self, limit_mb: u64) {
        self.inner
            .max_bytes
            .store(normalize_limit_mb(limit_mb) * MIB, Ordering::Relaxed);
        self.schedule_eviction();
    }

    /// Delete only regular files directly inside the known cover-cache
    /// directory. Subdirectories and anything outside this directory are never
    /// traversed or removed.
    pub fn clear(&self) -> AppResult<CacheInfo> {
        let entries = std::fs::read_dir(&self.inner.dir)
            .map_err(|e| AppError::Other(format!("cannot read cover cache: {e}")))?;
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        self.info()
    }

    /// Coalesce bursts of cover writes into at most one background scan. A
    /// later write schedules another pass if the first one already finished.
    pub fn schedule_eviction(&self) {
        if self.inner.eviction_running.swap(true, Ordering::AcqRel) {
            self.inner.eviction_pending.store(true, Ordering::Release);
            return;
        }
        let cache = self.clone();
        std::thread::spawn(move || {
            loop {
                cache.inner.eviction_pending.store(false, Ordering::Release);
                let max_bytes = cache.inner.max_bytes.load(Ordering::Relaxed);
                if let Err(error) = evict_files(&cache.inner.dir, max_bytes) {
                    tracing::debug!("cover cache eviction failed: {error}");
                }
                if !cache.inner.eviction_pending.load(Ordering::Acquire) {
                    break;
                }
            }
            cache.inner.eviction_running.store(false, Ordering::Release);
            // Close the small race where a writer requested a pass after the
            // final check but before `eviction_running` was released.
            if cache.inner.eviction_pending.swap(false, Ordering::AcqRel) {
                cache.schedule_eviction();
            }
        });
    }
}

pub fn normalize_limit_mb(limit_mb: u64) -> u64 {
    limit_mb.clamp(MIN_LIMIT_MB, MAX_LIMIT_MB)
}

fn scan_cache(dir: &Path) -> std::io::Result<(u64, u64)> {
    let mut size = 0u64;
    let mut count = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            size = size.saturating_add(metadata.len());
            count += 1;
        }
    }
    Ok((size, count))
}

fn evict_files(dir: &Path, max_bytes: u64) -> std::io::Result<()> {
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    entry.path(),
                    metadata.len(),
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                )
            })
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    if total <= max_bytes {
        return Ok(());
    }

    // Leave headroom so browsing does not trigger an eviction on every image.
    let target = max_bytes.saturating_mul(8) / 10;
    files.sort_by_key(|(_, _, modified)| *modified);
    let mut removed = 0u64;
    for (path, size, _) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!("cover cache eviction removed {removed} files");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cover_file_name, evict_files, scan_cache, write_file_atomic};
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        let path =
            std::env::temp_dir().join(format!("jellysic-cache-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create cache test directory");
        TempDir(path)
    }

    #[test]
    fn eviction_removes_oldest_file_and_leaves_headroom() {
        let dir = temp_dir();
        let old = dir.0.join("old.img");
        let new = dir.0.join("new.img");
        std::fs::write(&old, [0u8; 60]).unwrap();
        std::fs::write(&new, [0u8; 60]).unwrap();
        let old_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10);
        let new_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(20);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&new)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(new_time))
            .unwrap();

        evict_files(&dir.0, 100).unwrap();
        assert!(!old.exists());
        assert!(new.exists());
        assert_eq!(scan_cache(&dir.0).unwrap(), (60, 1));
    }

    #[test]
    fn atomic_write_replaces_the_file_and_leaves_no_temp_file() {
        let dir = temp_dir();
        let path = dir.0.join("cover.img");
        write_file_atomic(&path, b"first").unwrap();
        write_file_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(scan_cache(&dir.0).unwrap(), (6, 1));

        // A failed write (missing directory) leaves nothing behind either.
        assert!(write_file_atomic(&dir.0.join("missing").join("x.img"), b"x").is_err());
        assert_eq!(scan_cache(&dir.0).unwrap(), (6, 1));
    }

    #[test]
    fn cover_name_rejects_anything_that_could_steer_a_path() {
        // What the server legitimately sends.
        assert_eq!(
            cover_file_name("a1b2c3", "d4e5f6", 360).as_deref(),
            Some("a1b2c3-d4e5f6-360.img")
        );
        assert!(cover_file_name("with-dash_and_underscore", "tag", 96).is_some());

        // Everything that would leave the cache directory on Windows.
        for bad in [
            "..",
            "../etc",
            r"..\windows",
            r"C:\Windows\System32\x",
            r"\attacker\share\x",
            "a/b",
            "a:b",
            "a b",
            "a.b",
            "",
        ] {
            assert!(
                cover_file_name(bad, "tag", 360).is_none(),
                "item id {bad:?} must be rejected"
            );
            assert!(
                cover_file_name("item", bad, 360).is_none(),
                "tag {bad:?} must be rejected"
            );
        }

        // Length bound.
        assert!(cover_file_name(&"a".repeat(64), "tag", 360).is_some());
        assert!(cover_file_name(&"a".repeat(65), "tag", 360).is_none());
    }
}
