use crate::commands::{
    collision_filename, filename_len, sanitize_filename, truncate_to_filename_len, MAX_FILENAME_LEN,
};
use crate::error::{AppError, AppResult};
use crate::player::SharedSession;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, Semaphore};

const MAX_CONCURRENT: usize = 2;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
/// How often progress alone may send the task list to the UI. Every event
/// carries all tasks, finished ones included, so a tick from each running
/// download would re-send and re-render a long batch some 13 times a second.
/// A change of status still goes out at once.
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(500);
/// Ceiling for a single track when the server announces no `Content-Length`.
/// Far above any real audio file, including long high-resolution material.
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Tolerance over a declared `Content-Length` before the transfer is treated
/// as running away.
const DOWNLOAD_OVERRUN_SLACK: u64 = 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRequest {
    pub item_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTask {
    pub id: String,
    pub item_id: String,
    pub name: String,
    pub status: DownloadStatus,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub path: Option<String>,
    pub error: Option<String>,
}

struct ManagedTask {
    public: DownloadTask,
    cancel: Arc<AtomicBool>,
    cancel_wake: Arc<Notify>,
    attempt: u64,
}

struct DownloadInner {
    app: AppHandle,
    session: SharedSession,
    dir: PathBuf,
    semaphore: Arc<Semaphore>,
    tasks: Mutex<Vec<ManagedTask>>,
    reserved_paths: Mutex<HashSet<PathBuf>>,
    /// When the task list last went to the UI. Held while a list is taken and
    /// sent, so lists arrive in the order they were taken: two tasks finishing
    /// at once could otherwise leave the older list, with one of them still
    /// running, as the last word.
    last_emit: Mutex<Option<Instant>>,
}

#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadInner>,
}

enum RunError {
    Cancelled,
    Failed(String),
}

struct PartGuard {
    path: PathBuf,
    armed: bool,
}

impl PartGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl DownloadManager {
    pub fn new(app: AppHandle, session: SharedSession, dir: PathBuf) -> AppResult<Self> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Other(format!("cannot create downloads folder: {e}")))?;
        cleanup_stale_parts(&dir);
        Ok(Self {
            inner: Arc::new(DownloadInner {
                app,
                session,
                dir,
                semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT)),
                tasks: Mutex::new(Vec::new()),
                reserved_paths: Mutex::new(HashSet::new()),
                last_emit: Mutex::new(None),
            }),
        })
    }

    pub fn list(&self) -> Vec<DownloadTask> {
        self.inner
            .tasks
            .lock()
            .unwrap()
            .iter()
            .map(|task| task.public.clone())
            .collect()
    }

    pub fn enqueue(&self, requests: Vec<DownloadRequest>) -> Vec<DownloadTask> {
        let mut ids = Vec::new();
        {
            let mut tasks = self.inner.tasks.lock().unwrap();
            for request in requests {
                let item_id = request.item_id.trim().to_string();
                if item_id.is_empty() {
                    continue;
                }
                let id = uuid::Uuid::new_v4().to_string();
                ids.push(id.clone());
                tasks.push(ManagedTask {
                    public: DownloadTask {
                        id,
                        item_id,
                        name: display_name(&request.name),
                        status: DownloadStatus::Queued,
                        received_bytes: 0,
                        total_bytes: None,
                        path: None,
                        error: None,
                    },
                    cancel: Arc::new(AtomicBool::new(false)),
                    cancel_wake: Arc::new(Notify::new()),
                    attempt: 0,
                });
            }
        }
        self.emit();
        for id in &ids {
            self.spawn(id.clone(), 0);
        }
        let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        self.list()
            .into_iter()
            .filter(|task| id_set.contains(task.id.as_str()))
            .collect()
    }

    pub fn cancel(&self, id: &str) -> AppResult<()> {
        let mut tasks = self.inner.tasks.lock().unwrap();
        let task = tasks
            .iter_mut()
            .find(|task| task.public.id == id)
            .ok_or_else(|| AppError::Other("download not found".into()))?;
        if matches!(
            task.public.status,
            DownloadStatus::Queued | DownloadStatus::Downloading
        ) {
            task.cancel.store(true, Ordering::Release);
            task.cancel_wake.notify_one();
            task.public.status = DownloadStatus::Cancelled;
            task.public.error = None;
        }
        drop(tasks);
        self.emit();
        Ok(())
    }

    /// Signal every running download to stop. Used on the quit paths so the
    /// in-progress `.part` files are abandoned deliberately (the startup sweep
    /// removes them) instead of the process being pulled out from under them.
    pub fn cancel_all(&self) {
        let mut tasks = self.inner.tasks.lock().unwrap();
        for task in tasks.iter_mut() {
            if matches!(
                task.public.status,
                DownloadStatus::Queued | DownloadStatus::Downloading
            ) {
                task.cancel.store(true, Ordering::Release);
                task.cancel_wake.notify_one();
                task.public.status = DownloadStatus::Cancelled;
            }
        }
    }

    /// Cancel everything and forget the list, for a sign-out: neither the
    /// queued downloads nor the track names in the panel may carry over to
    /// whoever signs in next. Tasks still winding down find nothing left to
    /// update.
    pub fn reset(&self) {
        self.cancel_all();
        self.inner.tasks.lock().unwrap().clear();
        self.emit();
    }

    pub fn retry(&self, id: &str) -> AppResult<()> {
        let mut tasks = self.inner.tasks.lock().unwrap();
        let task = tasks
            .iter_mut()
            .find(|task| task.public.id == id)
            .ok_or_else(|| AppError::Other("download not found".into()))?;
        if !matches!(
            task.public.status,
            DownloadStatus::Failed | DownloadStatus::Cancelled
        ) {
            return Ok(());
        }
        task.cancel = Arc::new(AtomicBool::new(false));
        task.cancel_wake = Arc::new(Notify::new());
        task.attempt = task.attempt.saturating_add(1);
        let attempt = task.attempt;
        task.public.status = DownloadStatus::Queued;
        task.public.received_bytes = 0;
        task.public.total_bytes = None;
        task.public.path = None;
        task.public.error = None;
        drop(tasks);
        self.emit();
        self.spawn(id.to_string(), attempt);
        Ok(())
    }

    pub fn clear_finished(&self) {
        self.inner.tasks.lock().unwrap().retain(|task| {
            matches!(
                task.public.status,
                DownloadStatus::Queued | DownloadStatus::Downloading
            )
        });
        self.emit();
    }

    pub fn open_folder(&self) -> AppResult<()> {
        open_folder(&self.inner.dir)
    }

    fn spawn(&self, id: String, attempt: u64) {
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            manager.run(id, attempt).await;
        });
    }

    async fn run(&self, id: String, attempt: u64) {
        let (cancel, cancel_wake) = {
            let tasks = self.inner.tasks.lock().unwrap();
            let Some(task) = tasks
                .iter()
                .find(|task| task.public.id == id && task.attempt == attempt)
            else {
                return;
            };
            (task.cancel.clone(), task.cancel_wake.clone())
        };

        let permit = tokio::select! {
            permit = self.inner.semaphore.clone().acquire_owned() => permit,
            _ = cancel_wake.notified() => {
                self.finish(&id, attempt, Err(RunError::Cancelled));
                return;
            }
        };
        let Ok(_permit) = permit else {
            self.finish(
                &id,
                attempt,
                Err(RunError::Failed("download queue closed".into())),
            );
            return;
        };
        if cancel.load(Ordering::Acquire) {
            self.finish(&id, attempt, Err(RunError::Cancelled));
            return;
        }
        self.update(&id, attempt, |task| {
            task.status = DownloadStatus::Downloading
        });
        let result = self.stream(&id, attempt, &cancel, &cancel_wake).await;
        self.finish(&id, attempt, result);
    }

    async fn stream(
        &self,
        id: &str,
        attempt: u64,
        cancel: &AtomicBool,
        cancel_wake: &Notify,
    ) -> Result<PathBuf, RunError> {
        let item_id = self
            .task(id, attempt)
            .map(|task| task.item_id)
            .ok_or_else(|| RunError::Failed("download not found".into()))?;
        let client = self
            .inner
            .session
            .read()
            .await
            .clone()
            .ok_or_else(|| RunError::Failed(AppError::NotConnected.to_string()))?;
        // Race the request stage against cancellation too: the download
        // client's timeout is hours long, and a stalled server would otherwise
        // keep a cancelled task holding one of the concurrency slots.
        let (mut response, filename) = tokio::select! {
            result = client.download_item_stream(&item_id) => {
                result.map_err(|error| RunError::Failed(error.to_string()))?
            }
            _ = cancel_wake.notified() => return Err(RunError::Cancelled),
        };
        if cancel.load(Ordering::Acquire) {
            return Err(RunError::Cancelled);
        }

        let filename = sanitize_filename(&filename);
        let total = response.content_length();
        self.update(id, attempt, |task| {
            task.name = filename.clone();
            task.total_bytes = total;
        });
        let destination = self
            .reserve_destination(&filename)
            .map_err(|error| RunError::Failed(error.to_string()))?;
        let result = async {
            let part = part_path(&destination, id, attempt);
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)
                .await
                .map_err(|error| {
                    RunError::Failed(format!("cannot create partial download: {error}"))
                })?;
            let guard = PartGuard::new(part.clone());

            let mut received = 0u64;
            let mut last_emit = Instant::now();
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(RunError::Cancelled);
                }
                let chunk = tokio::select! {
                    chunk = response.chunk() => {
                        chunk.map_err(|error| RunError::Failed(error.to_string()))?
                    }
                    _ = cancel_wake.notified() => return Err(RunError::Cancelled),
                };
                let Some(chunk) = chunk else { break };
                received = received.saturating_add(chunk.len() as u64);
                // Stop a server that keeps sending past what it announced (or
                // never announced anything) from filling the disk.
                let ceiling = total.map_or(MAX_DOWNLOAD_BYTES, |declared| {
                    declared.saturating_add(DOWNLOAD_OVERRUN_SLACK)
                });
                if received > ceiling {
                    return Err(RunError::Failed(
                        "download exceeded the expected size".into(),
                    ));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|error| RunError::Failed(format!("download write failed: {error}")))?;
                if last_emit.elapsed() >= PROGRESS_INTERVAL {
                    self.update_progress(id, attempt, received);
                    last_emit = Instant::now();
                }
            }
            file.flush()
                .await
                .map_err(|error| RunError::Failed(format!("download flush failed: {error}")))?;
            file.sync_all()
                .await
                .map_err(|error| RunError::Failed(format!("download sync failed: {error}")))?;
            drop(file);
            if cancel.load(Ordering::Acquire) {
                return Err(RunError::Cancelled);
            }
            #[cfg(windows)]
            if let Err(error) = mark_from_internet(&part).await {
                tracing::debug!("cannot mark a download as from the internet: {error}");
            }
            let destination = finalize(&part, &destination)
                .await
                .map_err(|error| RunError::Failed(format!("download finalize failed: {error}")))?;
            guard.disarm();
            self.update_progress(id, attempt, received);
            Ok(destination)
        }
        .await;
        self.release_destination(&destination);
        result
    }

    fn task(&self, id: &str, attempt: u64) -> Option<DownloadTask> {
        self.inner
            .tasks
            .lock()
            .unwrap()
            .iter()
            .find(|task| task.public.id == id && task.attempt == attempt)
            .map(|task| task.public.clone())
    }

    fn update(&self, id: &str, attempt: u64, update: impl FnOnce(&mut DownloadTask)) {
        self.modify(id, attempt, update);
        self.emit();
    }

    /// Progress alone waits for [`PROGRESS_EMIT_INTERVAL`]; the next list to
    /// go out carries it either way.
    fn update_progress(&self, id: &str, attempt: u64, received: u64) {
        self.modify(id, attempt, |task| task.received_bytes = received);
        self.send_list(true);
    }

    fn modify(&self, id: &str, attempt: u64, update: impl FnOnce(&mut DownloadTask)) {
        if let Some(task) = self
            .inner
            .tasks
            .lock()
            .unwrap()
            .iter_mut()
            .find(|task| task.public.id == id && task.attempt == attempt)
        {
            update(&mut task.public);
        }
    }

    fn finish(&self, id: &str, attempt: u64, result: Result<PathBuf, RunError>) {
        self.update(id, attempt, |task| match result {
            Ok(path) => {
                task.status = DownloadStatus::Completed;
                task.path = Some(path.to_string_lossy().into_owned());
                task.error = None;
            }
            Err(RunError::Cancelled) => {
                task.status = DownloadStatus::Cancelled;
                task.path = None;
                task.error = None;
            }
            Err(RunError::Failed(error)) => {
                task.status = DownloadStatus::Failed;
                task.path = None;
                task.error = Some(error);
            }
        });
    }

    fn reserve_destination(&self, filename: &str) -> std::io::Result<PathBuf> {
        let mut reserved = self.inner.reserved_paths.lock().unwrap();
        for n in 0..1000u16 {
            let name = if n == 0 {
                filename.to_string()
            } else {
                collision_filename(filename, n)
            };
            let candidate = self.inner.dir.join(name);
            if !candidate.exists() && reserved.insert(candidate.clone()) {
                return Ok(candidate);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "too many files with the same download name",
        ))
    }

    fn release_destination(&self, path: &Path) {
        self.inner.reserved_paths.lock().unwrap().remove(path);
    }

    fn emit(&self) {
        self.send_list(false);
    }

    fn send_list(&self, progress_only: bool) {
        let mut last_emit = self.inner.last_emit.lock().unwrap();
        let now = Instant::now();
        if !emit_due(*last_emit, now, progress_only) {
            return;
        }
        let _ = self.inner.app.emit("download:state", self.list());
        *last_emit = Some(now);
    }
}

/// Whether a change to the task list goes out now: a change of status always
/// does, progress alone once [`PROGRESS_EMIT_INTERVAL`] has passed since the
/// last list was sent, whatever that one was for.
fn emit_due(last_emit: Option<Instant>, now: Instant, progress_only: bool) -> bool {
    !progress_only
        || last_emit
            .is_none_or(|last| now.saturating_duration_since(last) >= PROGRESS_EMIT_INTERVAL)
}

fn display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "Track".into()
    } else {
        name.chars().take(200).collect()
    }
}

/// Give the finished partial file its final name.
///
/// The name is claimed atomically instead of renamed onto: the reservation
/// happened before the transfer, which may have taken minutes — anything
/// could have created that file since, and `rename` replaces an existing
/// target on Windows without a word. Should the rename itself fail (a sync
/// client or scanner holding a file, a share that refuses), the empty
/// placeholder goes again: it would otherwise stay in Downloads under the
/// track's real name, and a retry would save next to it as " (1)".
async fn finalize(part: &Path, destination: &Path) -> std::io::Result<PathBuf> {
    let destination = claim_destination(destination).await?;
    if let Err(error) = tokio::fs::rename(part, &destination).await {
        // Only while it is still the empty file claim_destination created.
        if tokio::fs::metadata(&destination)
            .await
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0)
        {
            let _ = tokio::fs::remove_file(&destination).await;
        }
        return Err(error);
    }
    Ok(destination)
}

/// Take ownership of the target name at the last possible moment by creating
/// it with `create_new` — the only way to test-and-claim without a gap. If it
/// was taken while the download ran, fall back to the same " (n)" scheme the
/// reservation uses. The empty placeholder is immediately replaced by the
/// rename that follows (see [`finalize`]).
async fn claim_destination(destination: &Path) -> std::io::Result<PathBuf> {
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = destination.parent().unwrap_or(Path::new("."));
    for n in 0..1000u16 {
        let candidate = if n == 0 {
            destination.to_path_buf()
        } else {
            dir.join(collision_filename(&name, n))
        };
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many files with the same download name",
    ))
}

fn part_path(destination: &Path, id: &str, attempt: u64) -> PathBuf {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("track");
    // The attempt number keeps a retry's partial file distinct from a just-
    // cancelled run's still-unlinked one (same id), so `create_new` cannot
    // spuriously collide with it.
    let suffix = format!(".sonarune-{id}-{attempt}.part");
    // The partial name must fit one path component as well. The final name is
    // bounded already, but the suffix adds ~70 more: shorten the copy of the
    // name, never the suffix the startup sweep recognizes.
    let name =
        truncate_to_filename_len(name, MAX_FILENAME_LEN.saturating_sub(filename_len(&suffix)));
    destination.with_file_name(format!("{name}{suffix}"))
}

fn cleanup_stale_parts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= Duration::from_secs(24 * 60 * 60));
        if entry.file_type().is_ok_and(|kind| kind.is_file())
            // `.jellysic-` is what builds before the rename left behind.
            && (name.contains(".sonarune-") || name.contains(".jellysic-"))
            && name.ends_with(".part")
            && stale
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Mark a download the way a browser marks one (the Mark of the Web): its
/// `Zone.Identifier` stream says it came from the Internet zone, so Windows
/// applies its usual checks — SmartScreen, the "Open File - Security Warning"
/// prompt — should a server ever send something other than audio. Only the
/// zone: browsers also record the address, which here would name the server.
///
/// Written onto the partial file, so the stream moves along with the rename
/// and the file never sits under its real name unmarked. A drive without
/// streams (FAT, exFAT) refuses it, which the caller shrugs off.
#[cfg(windows)]
async fn mark_from_internet(path: &Path) -> std::io::Result<()> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    tokio::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n").await
}

/// Explorer, by its full path. Started by name, Windows would look in the
/// folder of our own exe first — for the portable build that is often
/// Downloads itself, and an `explorer.exe` someone left there would run
/// instead.
#[cfg(target_os = "windows")]
pub(crate) fn explorer() -> std::process::Command {
    std::process::Command::new(windows_dir().join("explorer.exe"))
}

#[cfg(target_os = "windows")]
fn windows_dir() -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

    let mut buffer = [0u16; 260];
    // SAFETY: the buffer is valid for writes of the length passed with it.
    let len = unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    match usize::try_from(len) {
        Ok(len) if len > 0 && len < buffer.len() => {
            PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len]))
        }
        _ => std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows")),
    }
}

fn open_folder(path: &Path) -> AppResult<()> {
    #[cfg(target_os = "windows")]
    let mut command = explorer();
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");

    command
        .arg(path)
        .spawn()
        .map_err(|e| AppError::Other(format!("cannot open downloads folder: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{display_name, emit_due, finalize, part_path, PROGRESS_EMIT_INTERVAL};
    use crate::commands::{collision_filename, filename_len, sanitize_filename, MAX_FILENAME_LEN};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sonarune-download-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn progress_alone_goes_out_at_most_every_interval() {
        let start = Instant::now();
        // Nothing sent yet: the first progress goes out.
        assert!(emit_due(None, start, true));
        let soon = start + Duration::from_millis(150);
        assert!(!emit_due(Some(start), soon, true));
        assert!(emit_due(Some(start), start + PROGRESS_EMIT_INTERVAL, true));
        // A change of status never waits.
        assert!(emit_due(Some(start), soon, false));
        assert!(emit_due(Some(start), start, false));
    }

    #[tokio::test]
    async fn a_failed_finalize_leaves_no_empty_file_behind() {
        let dir = temp_dir("finalize");
        let destination = dir.join("Song.flac");
        // No partial file to move, so the rename fails after the name was
        // claimed.
        let result = finalize(&dir.join("missing.part"), &destination).await;
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_err());
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    #[tokio::test]
    async fn finalize_moves_the_partial_file_onto_a_free_name() {
        let dir = temp_dir("finalize-ok");
        let part = dir.join("Song.flac.sonarune-x-0.part");
        std::fs::write(&part, b"audio").unwrap();
        // Taken while the download ran: the file goes next to it.
        std::fs::write(dir.join("Song.flac"), b"other").unwrap();
        let result = finalize(&part, &dir.join("Song.flac")).await;
        let moved = result
            .as_ref()
            .ok()
            .map(|path| std::fs::read(path).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(result.unwrap(), dir.join("Song (1).flac"));
        assert_eq!(moved.as_deref(), Some(&b"audio"[..]));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn downloads_carry_the_mark_of_the_web() {
        let dir = temp_dir("motw");
        let file = dir.join("Song.flac.sonarune-x-0.part");
        std::fs::write(&file, b"audio").unwrap();
        let marked = super::mark_from_internet(&file).await;
        let mut stream = file.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let zone = std::fs::read_to_string(PathBuf::from(stream));
        let _ = std::fs::remove_dir_all(&dir);
        marked.expect("write the Zone.Identifier stream");
        assert_eq!(zone.unwrap(), "[ZoneTransfer]\r\nZoneId=3\r\n");
    }

    #[cfg(windows)]
    #[test]
    fn explorer_is_started_by_its_full_path() {
        let command = super::explorer();
        let program = Path::new(command.get_program());
        assert!(program.is_absolute(), "{program:?}");
        assert_eq!(
            program.file_name().and_then(|name| name.to_str()),
            Some("explorer.exe")
        );
        assert!(program.is_file(), "{program:?}");
    }

    #[test]
    fn long_names_yield_creatable_final_and_partial_files() {
        // Emoji are two UTF-16 units each (NTFS counts units, not chars).
        let server_name = format!("{}.flac", "🎵".repeat(300));
        let name = sanitize_filename(&server_name);
        let collided = collision_filename(&name, 999);
        assert!(filename_len(&collided) <= MAX_FILENAME_LEN, "{collided}");
        assert!(collided.ends_with(".flac"));

        let id = uuid::Uuid::new_v4().to_string();
        let part = part_path(&Path::new("downloads").join(&collided), &id, u64::MAX);
        let part_name = part.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(filename_len(part_name) <= MAX_FILENAME_LEN, "{part_name}");
        assert!(part_name.encode_utf16().count() <= 255);
        assert!(part_name.ends_with(&format!(".sonarune-{id}-{}.part", u64::MAX)));

        // And the file system agrees (this used to fail with os error 123).
        let dir = std::env::temp_dir().join(format!("sonarune-part-test-{id}"));
        std::fs::create_dir(&dir).unwrap();
        let created = [collided.as_str(), part_name]
            .map(|file| std::fs::File::create(dir.join(file)).map(drop));
        let _ = std::fs::remove_dir_all(&dir);
        for result in created {
            result.expect("create file with a bounded name");
        }
    }

    #[test]
    fn partial_file_stays_next_to_destination() {
        let path = part_path(Path::new(r"C:\Downloads\song.flac"), "task-id", 0);
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("song.flac.sonarune-task-id-0.part")
        );
    }

    #[test]
    fn retry_uses_a_distinct_partial_file() {
        let dest = Path::new(r"C:\Downloads\song.flac");
        assert_ne!(part_path(dest, "task-id", 0), part_path(dest, "task-id", 1));
    }

    #[test]
    fn display_name_is_bounded_and_has_fallback() {
        assert_eq!(display_name("  "), "Track");
        assert_eq!(display_name(&"a".repeat(250)).chars().count(), 200);
    }
}
