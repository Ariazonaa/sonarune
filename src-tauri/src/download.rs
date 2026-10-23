use crate::api::JellyfinClient;
use crate::commands::{
    collision_filename, filename_len, sanitize_filename, truncate_to_filename_len, MAX_FILENAME_LEN,
};
use crate::error::{AppError, AppResult, ErrorReport};
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
/// How often progress alone may go to the UI: a tick from each running
/// download would otherwise send some 13 events a second. A change of status
/// still goes out at once, and progress held back goes out at the latest when
/// the interval is over, even if no further tick comes (a stalled server).
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTask {
    pub id: String,
    pub item_id: String,
    pub name: String,
    pub status: DownloadStatus,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub path: Option<String>,
    /// Why it failed, worded by the UI (`src/lib/appErrors.ts`).
    pub error: Option<ErrorReport>,
    /// The list's revision at this task's last change. The UI keeps the newer
    /// of two copies of a task by it, whichever way they reached it.
    pub revision: u64,
}

/// The whole task list: the `download:state` event and what `get_downloads`
/// answers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSnapshot {
    /// The list's revision when it was taken. Every change raises it, so a
    /// list older than one the UI already has can be told apart.
    pub revision: u64,
    pub tasks: Vec<DownloadTask>,
}

/// What goes to the UI.
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    /// `download:state`: the whole list, after a change to which tasks there
    /// are (added, cleared, dropped at sign-out).
    State(DownloadSnapshot),
    /// `download:changed`: only the tasks that changed since the last event —
    /// progress, status, name, size. A batch of thousands costs per event
    /// what its running downloads cost, not what the whole list does.
    Changed(Vec<DownloadTask>),
}

type EventSink = Box<dyn Fn(DownloadEvent) + Send + Sync>;

/// Renews a revoked session (`commands::renew_session`). Handed in like the
/// sink, so tests reach `request` without linking the app's runtime: the
/// test binary has no manifest for the common controls it would pull in.
type Renewal = Box<
    dyn Fn(
            Arc<JellyfinClient>,
            Instant,
            AppError,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = AppResult<Arc<JellyfinClient>>> + Send>,
        > + Send
        + Sync,
>;

struct ManagedTask {
    public: DownloadTask,
    cancel: Arc<AtomicBool>,
    cancel_wake: Arc<Notify>,
    attempt: u64,
}

/// The tasks, with what the UI has not been told yet.
#[derive(Default)]
struct TaskList {
    tasks: Vec<ManagedTask>,
    /// Raised by every change. A changed task carries the value it was
    /// raised to.
    revision: u64,
    /// Tasks changed since the last event went out.
    changed: HashSet<String>,
}

impl TaskList {
    fn snapshot(&self) -> DownloadSnapshot {
        DownloadSnapshot {
            revision: self.revision,
            tasks: self.tasks.iter().map(|task| task.public.clone()).collect(),
        }
    }

    fn position(&self, id: &str, attempt: u64) -> Option<usize> {
        self.tasks
            .iter()
            .position(|task| task.public.id == id && task.attempt == attempt)
    }

    /// Apply `update` to the task at `index`. Only a real change raises the
    /// revision and waits to be sent: a cancelled download that winds down
    /// and reports "cancelled" once more sends nothing.
    fn modify_at(&mut self, index: usize, update: impl FnOnce(&mut DownloadTask)) -> bool {
        let Some(task) = self.tasks.get_mut(index) else {
            return false;
        };
        let before = task.public.clone();
        update(&mut task.public);
        if task.public == before {
            return false;
        }
        self.revision += 1;
        task.public.revision = self.revision;
        self.changed.insert(task.public.id.clone());
        true
    }

    /// The tasks changed since the last event, in list order.
    fn take_changed(&mut self) -> Vec<DownloadTask> {
        if self.changed.is_empty() {
            return Vec::new();
        }
        let changed = std::mem::take(&mut self.changed);
        self.tasks
            .iter()
            .filter(|task| changed.contains(&task.public.id))
            .map(|task| task.public.clone())
            .collect()
    }
}

/// What the UI is sent next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emit {
    /// The whole list: which tasks there are changed.
    List,
    /// The changed tasks, now: a status changed.
    Changes,
    /// The changed tasks, once [`PROGRESS_EMIT_INTERVAL`] allows.
    Progress,
}

#[derive(Default)]
struct EmitState {
    /// When the last event went out.
    last: Option<Instant>,
    /// A send of held-back progress is scheduled.
    flush_scheduled: bool,
}

struct DownloadInner {
    sink: EventSink,
    /// For a revoked session (`request`); tests run without one.
    renewal: Option<Renewal>,
    session: SharedSession,
    dir: PathBuf,
    semaphore: Arc<Semaphore>,
    tasks: Mutex<TaskList>,
    reserved_paths: Mutex<HashSet<PathBuf>>,
    /// Held while an event is taken and sent, so events arrive in the order
    /// they were taken: two tasks finishing at once could otherwise leave the
    /// older word, with one of them still running, as the last one. Locked
    /// before `tasks`, never while holding it.
    emit: Mutex<EmitState>,
}

#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadInner>,
}

enum RunError {
    Cancelled,
    Failed(AppError),
}

/// A failure of the download's own, in words `error.rs` gives a code.
fn failed(message: impl Into<String>) -> RunError {
    RunError::Failed(AppError::Other(message.into()))
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
        let events = app.clone();
        let sink = move |event: DownloadEvent| {
            let _ = match event {
                DownloadEvent::State(snapshot) => events.emit("download:state", snapshot),
                DownloadEvent::Changed(tasks) => events.emit("download:changed", tasks),
            };
        };
        let renewal: Renewal = Box::new(move |client, at, unauthorized| {
            let app = app.clone();
            Box::pin(async move {
                crate::commands::renew_session(&app, &client, at, unauthorized).await
            })
        });
        Self::with_sink(Box::new(sink), Some(renewal), session, dir)
    }

    fn with_sink(
        sink: EventSink,
        renewal: Option<Renewal>,
        session: SharedSession,
        dir: PathBuf,
    ) -> AppResult<Self> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Other(format!("cannot create downloads folder: {e}")))?;
        cleanup_stale_parts(&dir);
        Ok(Self {
            inner: Arc::new(DownloadInner {
                sink,
                renewal,
                session,
                dir,
                semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT)),
                tasks: Mutex::new(TaskList::default()),
                reserved_paths: Mutex::new(HashSet::new()),
                emit: Mutex::new(EmitState::default()),
            }),
        })
    }

    /// The whole list with its revision, as the UI reads it on start and
    /// when its window comes back.
    pub fn snapshot(&self) -> DownloadSnapshot {
        self.inner.tasks.lock().unwrap().snapshot()
    }

    pub fn enqueue(&self, requests: Vec<DownloadRequest>) -> Vec<DownloadTask> {
        let added = self.add(requests);
        for task in &added {
            self.spawn(task.id.clone(), 0);
        }
        added
    }

    /// Put the requested tracks on the list, queued, and send the list.
    fn add(&self, requests: Vec<DownloadRequest>) -> Vec<DownloadTask> {
        let mut added = Vec::new();
        {
            let mut list = self.inner.tasks.lock().unwrap();
            list.revision += 1;
            let revision = list.revision;
            for request in requests {
                let item_id = request.item_id.trim().to_string();
                if item_id.is_empty() {
                    continue;
                }
                let public = DownloadTask {
                    id: uuid::Uuid::new_v4().to_string(),
                    item_id,
                    name: display_name(&request.name),
                    status: DownloadStatus::Queued,
                    received_bytes: 0,
                    total_bytes: None,
                    path: None,
                    error: None,
                    revision,
                };
                added.push(public.clone());
                list.tasks.push(ManagedTask {
                    public,
                    cancel: Arc::new(AtomicBool::new(false)),
                    cancel_wake: Arc::new(Notify::new()),
                    attempt: 0,
                });
            }
        }
        self.send(Emit::List);
        added
    }

    pub fn cancel(&self, id: &str) -> AppResult<()> {
        let mut list = self.inner.tasks.lock().unwrap();
        let index = list
            .tasks
            .iter()
            .position(|task| task.public.id == id)
            .ok_or_else(|| AppError::Other("download not found".into()))?;
        cancel_at(&mut list, index);
        drop(list);
        self.send(Emit::Changes);
        Ok(())
    }

    /// Signal every running download to stop. Used on the quit paths so the
    /// in-progress `.part` files are abandoned deliberately (the startup sweep
    /// removes them) instead of the process being pulled out from under them.
    /// Sends nothing itself, as it runs on the quit paths: after a blocked
    /// update, the first download that winds down carries all of them to the
    /// panel.
    pub fn cancel_all(&self) {
        let mut list = self.inner.tasks.lock().unwrap();
        for index in 0..list.tasks.len() {
            cancel_at(&mut list, index);
        }
    }

    /// Cancel everything and forget the list, for a sign-out: neither the
    /// queued downloads nor the track names in the panel may carry over to
    /// whoever signs in next. Tasks still winding down find nothing left to
    /// update.
    pub fn reset(&self) {
        self.cancel_all();
        {
            let mut list = self.inner.tasks.lock().unwrap();
            list.tasks.clear();
            list.revision += 1;
        }
        self.send(Emit::List);
    }

    pub fn retry(&self, id: &str) -> AppResult<()> {
        let mut list = self.inner.tasks.lock().unwrap();
        let index = list
            .tasks
            .iter()
            .position(|task| task.public.id == id)
            .ok_or_else(|| AppError::Other("download not found".into()))?;
        let Some(attempt) = requeue_at(&mut list, index) else {
            return Ok(());
        };
        drop(list);
        self.send(Emit::Changes);
        self.spawn(id.to_string(), attempt);
        Ok(())
    }

    /// Retry every failed and cancelled download at once — a batch of
    /// thousands in one call, and what an update hand-over cancelled.
    /// Returns how many went back into the queue.
    pub fn retry_failed(&self) -> usize {
        let mut list = self.inner.tasks.lock().unwrap();
        let mut runs = Vec::new();
        for index in 0..list.tasks.len() {
            if let Some(attempt) = requeue_at(&mut list, index) {
                runs.push((list.tasks[index].public.id.clone(), attempt));
            }
        }
        drop(list);
        self.send(Emit::Changes);
        let count = runs.len();
        for (id, attempt) in runs {
            self.spawn(id, attempt);
        }
        count
    }

    pub fn clear_finished(&self) {
        {
            let mut list = self.inner.tasks.lock().unwrap();
            list.tasks.retain(|task| {
                matches!(
                    task.public.status,
                    DownloadStatus::Queued | DownloadStatus::Downloading
                )
            });
            list.revision += 1;
        }
        self.send(Emit::List);
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
            let list = self.inner.tasks.lock().unwrap();
            let Some(index) = list.position(&id, attempt) else {
                return;
            };
            let task = &list.tasks[index];
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
            self.finish(&id, attempt, Err(failed("download queue closed")));
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
            .ok_or_else(|| failed("download not found"))?;
        let client = self
            .inner
            .session
            .read()
            .await
            .clone()
            .ok_or(RunError::Failed(AppError::NotConnected))?;
        // Race the request stage against cancellation too: the download
        // client's timeout is hours long, and a stalled server would otherwise
        // keep a cancelled task holding one of the concurrency slots.
        let (mut response, filename) = tokio::select! {
            result = self.request(client, &item_id) => result.map_err(RunError::Failed)?,
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
            .map_err(|error| failed(error.to_string()))?;
        let result = async {
            let part = part_path(&destination, id, attempt);
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)
                .await
                .map_err(|error| failed(format!("cannot create partial download: {error}")))?;
            let guard = PartGuard::new(part.clone());

            let mut received = 0u64;
            let mut last_emit = Instant::now();
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(RunError::Cancelled);
                }
                let chunk = tokio::select! {
                    // Through `AppError`, which strips the URL off a body error.
                    chunk = response.chunk() => chunk.map_err(|error| RunError::Failed(error.into()))?,
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
                    return Err(failed("download exceeded the expected size"));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|error| failed(format!("download write failed: {error}")))?;
                if last_emit.elapsed() >= PROGRESS_INTERVAL {
                    self.update_progress(id, attempt, received);
                    last_emit = Instant::now();
                }
            }
            file.flush()
                .await
                .map_err(|error| failed(format!("download flush failed: {error}")))?;
            file.sync_all()
                .await
                .map_err(|error| failed(format!("download sync failed: {error}")))?;
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
                .map_err(|error| failed(format!("download finalize failed: {error}")))?;
            guard.disarm();
            self.update_progress(id, attempt, received);
            Ok(destination)
        }
        .await;
        self.release_destination(&destination);
        result
    }

    /// Ask for the file. A revoked session gets one renewal, the way every
    /// caller renews it (`commands::renew_session`), and one more request.
    async fn request(
        &self,
        client: Arc<JellyfinClient>,
        item_id: &str,
    ) -> AppResult<(reqwest::Response, String)> {
        match client.download_item_stream(item_id).await {
            Err(unauthorized @ AppError::Server { status: 401, .. }) => {
                let Some(renewal) = &self.inner.renewal else {
                    return Err(unauthorized);
                };
                let renewed = renewal(client, Instant::now(), unauthorized).await?;
                renewed.download_item_stream(item_id).await
            }
            other => other,
        }
    }

    fn task(&self, id: &str, attempt: u64) -> Option<DownloadTask> {
        let list = self.inner.tasks.lock().unwrap();
        list.position(id, attempt)
            .map(|index| list.tasks[index].public.clone())
    }

    fn update(&self, id: &str, attempt: u64, update: impl FnOnce(&mut DownloadTask)) {
        self.modify(id, attempt, update);
        // Even when this changed nothing, what is pending goes out: after
        // `cancel_all`, the runs winding down only repeat "cancelled".
        self.send(Emit::Changes);
    }

    /// Progress alone waits for [`PROGRESS_EMIT_INTERVAL`]; whatever goes
    /// out next carries it either way.
    fn update_progress(&self, id: &str, attempt: u64, received: u64) {
        if self.modify(id, attempt, |task| task.received_bytes = received) {
            self.send(Emit::Progress);
        }
    }

    fn modify(&self, id: &str, attempt: u64, update: impl FnOnce(&mut DownloadTask)) -> bool {
        let mut list = self.inner.tasks.lock().unwrap();
        list.position(id, attempt)
            .is_some_and(|index| list.modify_at(index, update))
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
                let report = error.report();
                tracing::debug!(code = %report.code, "download failed: {}", report.detail);
                task.status = DownloadStatus::Failed;
                task.path = None;
                task.error = Some(report);
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

    fn send(&self, kind: Emit) {
        let mut emit = self.inner.emit.lock().unwrap();
        self.send_locked(&mut emit, kind);
    }

    fn send_locked(&self, emit: &mut EmitState, kind: Emit) {
        let now = Instant::now();
        let event = match kind {
            Emit::List => {
                let mut list = self.inner.tasks.lock().unwrap();
                // The list carries every change.
                list.changed.clear();
                DownloadEvent::State(list.snapshot())
            }
            Emit::Changes | Emit::Progress => {
                let mut list = self.inner.tasks.lock().unwrap();
                if list.changed.is_empty() {
                    return;
                }
                if kind == Emit::Progress {
                    if let Some(wait) = progress_wait(emit.last, now) {
                        drop(list);
                        self.schedule_flush(emit, wait);
                        return;
                    }
                }
                DownloadEvent::Changed(list.take_changed())
            }
        };
        (self.inner.sink)(event);
        emit.last = Some(now);
    }

    /// Send held-back progress once the interval is over, should nothing else
    /// carry it before.
    fn schedule_flush(&self, emit: &mut EmitState, wait: Duration) {
        if emit.flush_scheduled {
            return;
        }
        emit.flush_scheduled = true;
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(wait).await;
            let mut emit = manager.inner.emit.lock().unwrap();
            emit.flush_scheduled = false;
            manager.send_locked(&mut emit, Emit::Progress);
        });
    }
}

/// Cancel the task at `index` if it is still to run or running.
fn cancel_at(list: &mut TaskList, index: usize) {
    let task = &list.tasks[index];
    if !matches!(
        task.public.status,
        DownloadStatus::Queued | DownloadStatus::Downloading
    ) {
        return;
    }
    task.cancel.store(true, Ordering::Release);
    task.cancel_wake.notify_one();
    list.modify_at(index, |task| {
        task.status = DownloadStatus::Cancelled;
        task.error = None;
    });
}

/// Put the failed or cancelled task at `index` back into the queue under a
/// new attempt, which its run is spawned with; None for any other task.
fn requeue_at(list: &mut TaskList, index: usize) -> Option<u64> {
    let task = &mut list.tasks[index];
    if !matches!(
        task.public.status,
        DownloadStatus::Failed | DownloadStatus::Cancelled
    ) {
        return None;
    }
    task.cancel = Arc::new(AtomicBool::new(false));
    task.cancel_wake = Arc::new(Notify::new());
    task.attempt = task.attempt.saturating_add(1);
    let attempt = task.attempt;
    list.modify_at(index, |task| {
        task.status = DownloadStatus::Queued;
        task.received_bytes = 0;
        task.total_bytes = None;
        task.path = None;
        task.error = None;
    });
    Some(attempt)
}

/// How long progress alone still has to wait: nothing once
/// [`PROGRESS_EMIT_INTERVAL`] has passed since the last event, whatever that
/// one was for. A change of status never waits.
fn progress_wait(last_emit: Option<Instant>, now: Instant) -> Option<Duration> {
    let elapsed = now.saturating_duration_since(last_emit?);
    PROGRESS_EMIT_INTERVAL
        .checked_sub(elapsed)
        .filter(|wait| !wait.is_zero())
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
    use super::{
        display_name, failed, finalize, part_path, progress_wait, DownloadEvent, DownloadManager,
        DownloadRequest, DownloadStatus, DownloadTask, RunError, PROGRESS_EMIT_INTERVAL,
    };
    use crate::commands::{collision_filename, filename_len, sanitize_filename, MAX_FILENAME_LEN};
    use crate::player::SharedSession;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sonarune-download-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    type Events = Arc<Mutex<Vec<DownloadEvent>>>;

    /// A manager without a session that records what it would send the UI.
    fn recording(tag: &str) -> (DownloadManager, Events, PathBuf) {
        let dir = temp_dir(tag);
        let events: Events = Arc::default();
        let sink = events.clone();
        let session: SharedSession = Arc::new(tokio::sync::RwLock::new(None));
        let manager = DownloadManager::with_sink(
            Box::new(move |event| sink.lock().unwrap().push(event)),
            None,
            session,
            dir.clone(),
        )
        .unwrap();
        (manager, events, dir)
    }

    fn requests(count: usize) -> Vec<DownloadRequest> {
        (0..count)
            .map(|n| DownloadRequest {
                item_id: format!("item-{n}"),
                name: format!("Track {n}"),
            })
            .collect()
    }

    fn take(events: &Events) -> Vec<DownloadEvent> {
        std::mem::take(&mut *events.lock().unwrap())
    }

    fn changed(event: &DownloadEvent) -> &[DownloadTask] {
        match event {
            DownloadEvent::Changed(tasks) => tasks,
            DownloadEvent::State(_) => panic!("expected changes, got the whole list"),
        }
    }

    fn start(manager: &DownloadManager, id: &str) {
        manager.update(id, 0, |task| task.status = DownloadStatus::Downloading);
    }

    /// What the UI makes of the events (`downloads.svelte.ts`): a list older
    /// than the one it holds is dropped, and of two copies of a task the one
    /// with the higher revision wins.
    fn replay<'a>(events: impl IntoIterator<Item = &'a DownloadEvent>) -> Vec<DownloadTask> {
        let mut base = 0;
        let mut tasks: Vec<DownloadTask> = Vec::new();
        for event in events {
            match event {
                DownloadEvent::State(snapshot) => {
                    if snapshot.revision < base {
                        continue;
                    }
                    let held: HashMap<&str, &DownloadTask> =
                        tasks.iter().map(|task| (task.id.as_str(), task)).collect();
                    tasks = snapshot
                        .tasks
                        .iter()
                        .map(|task| match held.get(task.id.as_str()) {
                            Some(mine) if mine.revision > task.revision => (*mine).clone(),
                            _ => task.clone(),
                        })
                        .collect();
                    base = snapshot.revision;
                }
                DownloadEvent::Changed(changes) => {
                    for change in changes {
                        if let Some(mine) = tasks.iter_mut().find(|task| task.id == change.id) {
                            if change.revision > mine.revision {
                                *mine = change.clone();
                            }
                        }
                    }
                }
            }
        }
        tasks
    }

    #[test]
    fn progress_alone_goes_out_at_most_every_interval() {
        let start = Instant::now();
        // Nothing sent yet: the first progress goes out.
        assert_eq!(progress_wait(None, start), None);
        let soon = start + Duration::from_millis(150);
        assert_eq!(
            progress_wait(Some(start), soon),
            Some(PROGRESS_EMIT_INTERVAL - Duration::from_millis(150))
        );
        assert_eq!(
            progress_wait(Some(start), start + PROGRESS_EMIT_INTERVAL),
            None
        );
        assert_eq!(
            progress_wait(Some(start), start + 2 * PROGRESS_EMIT_INTERVAL),
            None
        );
    }

    #[test]
    fn a_batch_goes_out_once_whole_and_then_only_as_changes() {
        let (manager, events, dir) = recording("batch");
        let added = manager.add(requests(500));
        let sent = take(&events);
        assert_eq!(sent.len(), 1);
        let DownloadEvent::State(list) = &sent[0] else {
            panic!("a new batch goes out as the whole list");
        };
        assert_eq!(list.tasks.len(), 500);
        assert!(added.iter().all(|task| task.revision <= list.revision));

        // Starting and finishing one download sends that one task, never the
        // other 499.
        start(&manager, &added[0].id);
        manager.update_progress(&added[0].id, 0, 1024);
        manager.finish(&added[0].id, 0, Ok(dir.join("Track 0.flac")));
        let sent = take(&events);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!sent.is_empty());
        for event in &sent {
            let tasks = changed(event);
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].id, added[0].id);
        }
        let last = changed(sent.last().unwrap());
        assert_eq!(last[0].status, DownloadStatus::Completed);
        assert_eq!(last[0].received_bytes, 1024);
    }

    #[test]
    fn progress_held_back_goes_out_when_the_interval_is_over() {
        let (manager, events, dir) = recording("flush");
        let id = manager.add(requests(1))[0].id.clone();
        start(&manager, &id);
        take(&events);

        // Just after the start went out: held back, and the later ticks
        // replace the earlier ones.
        manager.update_progress(&id, 0, 100);
        manager.update_progress(&id, 0, 200);
        manager.update_progress(&id, 0, 300);
        assert!(take(&events).is_empty());

        // No further tick comes (a stalled server): the last one still
        // arrives, once.
        let deadline = Instant::now() + Duration::from_secs(5);
        while events.lock().unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "the held-back progress never went out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(PROGRESS_EMIT_INTERVAL);
        let sent = take(&events);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(sent.len(), 1, "{sent:?}");
        let tasks = changed(&sent[0]);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].received_bytes, 300);
    }

    #[test]
    fn a_failure_reaches_the_ui_as_a_code() {
        let (manager, events, dir) = recording("code");
        let added = manager.add(requests(1));
        let id = added[0].id.clone();
        start(&manager, &id);
        take(&events);
        manager.finish(
            &id,
            0,
            Err(failed(
                "download write failed: There is not enough space on the disk. (os error 112)",
            )),
        );
        let sent = take(&events);
        let _ = std::fs::remove_dir_all(&dir);
        let error = changed(&sent[0])[0].error.clone().expect("an error");
        assert_eq!(error.code, "downloadWrite");
        assert_eq!(
            error.reason.as_deref(),
            Some("There is not enough space on the disk. (os error 112)")
        );
    }

    #[test]
    fn a_change_of_status_carries_the_progress_held_back() {
        let (manager, events, dir) = recording("carry");
        let added = manager.add(requests(2));
        let (a, b) = (added[0].id.clone(), added[1].id.clone());
        start(&manager, &a);
        start(&manager, &b);
        take(&events);

        manager.update_progress(&a, 0, 4096);
        assert!(take(&events).is_empty());
        manager.finish(&b, 0, Err(failed("server returned 500")));
        let sent = take(&events);
        assert_eq!(sent.len(), 1);
        let tasks = changed(&sent[0]);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].received_bytes, 4096);
        assert_eq!(tasks[1].status, DownloadStatus::Failed);
        // A message of no known kind keeps its English for the UI to show.
        let error = tasks[1].error.as_ref().expect("an error");
        assert_eq!(
            (error.code.as_ref(), error.detail.as_str()),
            ("other", "server returned 500")
        );

        // Nothing is left for the scheduled send.
        std::thread::sleep(PROGRESS_EMIT_INTERVAL + Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(take(&events).is_empty());
    }

    #[test]
    fn a_task_that_did_not_change_sends_nothing() {
        let (manager, events, dir) = recording("noop");
        let id = manager.add(requests(1))[0].id.clone();
        take(&events);
        manager.cancel(&id).unwrap();
        assert_eq!(
            changed(&take(&events)[0])[0].status,
            DownloadStatus::Cancelled
        );
        // The cancelled run winds down and says so once more; a second press.
        manager.finish(&id, 0, Err(RunError::Cancelled));
        manager.cancel(&id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(take(&events).is_empty());
    }

    #[test]
    fn cancelling_everything_reaches_the_panel_with_the_first_run_to_stop() {
        // The update hand-over cancels all and sends nothing; should the
        // installer not start, the panel must not show them running.
        let (manager, events, dir) = recording("cancel-all");
        let added = manager.add(requests(3));
        start(&manager, &added[0].id);
        take(&events);
        manager.cancel_all();
        assert!(take(&events).is_empty());
        manager.finish(&added[0].id, 0, Err(RunError::Cancelled));
        let sent = take(&events);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(sent.len(), 1);
        let tasks = changed(&sent[0]);
        assert_eq!(tasks.len(), 3);
        assert!(tasks
            .iter()
            .all(|task| task.status == DownloadStatus::Cancelled));
    }

    #[test]
    fn retrying_all_requeues_the_failed_and_cancelled_in_one_go() {
        let (manager, events, dir) = recording("retry-all");
        let added = manager.add(requests(5));
        let ids: Vec<String> = added.iter().map(|task| task.id.clone()).collect();
        manager.finish(&ids[0], 0, Ok(dir.join("Track 0.flac")));
        manager.finish(&ids[1], 0, Err(failed("server returned 500")));
        manager.cancel(&ids[2]).unwrap();
        manager.finish(&ids[3], 0, Err(failed("timed out")));
        take(&events);

        assert_eq!(manager.retry_failed(), 3);
        let sent = take(&events);
        // One event for the lot, before any run starts.
        let first = changed(&sent[0]);
        let requeued: Vec<&str> = first.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(
            requeued,
            [ids[1].as_str(), ids[2].as_str(), ids[3].as_str()]
        );
        assert!(first
            .iter()
            .all(|task| task.status == DownloadStatus::Queued && task.error.is_none()));

        // The runs start under the new attempt: without a session they fail
        // again, and say so.
        let deadline = Instant::now() + Duration::from_secs(10);
        let failed_again = || {
            let mut all = sent.clone();
            all.extend(events.lock().unwrap().iter().cloned());
            let tasks = replay(
                std::iter::once(&DownloadEvent::State(super::DownloadSnapshot {
                    revision: 0,
                    tasks: added.clone(),
                }))
                .chain(all.iter()),
            );
            [1, 2, 3]
                .iter()
                .all(|&n| tasks[n].status == DownloadStatus::Failed)
        };
        while !failed_again() {
            assert!(
                Instant::now() < deadline,
                "the retried downloads did not run"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let truth = manager.snapshot();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(truth.tasks[0].status, DownloadStatus::Completed);
        assert_eq!(truth.tasks[4].status, DownloadStatus::Queued);
        assert_eq!(manager.retry_failed(), 3);
    }

    #[test]
    fn clearing_sends_the_whole_list_newer_than_every_task() {
        let (manager, events, dir) = recording("clear");
        let added = manager.add(requests(3));
        manager.finish(&added[1].id, 0, Ok(dir.join("Track 1.flac")));
        let newest = changed(take(&events).last().unwrap())[0].revision;
        manager.clear_finished();
        let sent = take(&events);
        let _ = std::fs::remove_dir_all(&dir);
        let [DownloadEvent::State(list)] = &sent[..] else {
            panic!("clearing sends the whole list: {sent:?}");
        };
        assert!(list.revision > newest);
        let ids: Vec<&str> = list.tasks.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, [added[0].id.as_str(), added[2].id.as_str()]);
    }

    #[test]
    fn the_changes_rebuild_the_list_in_any_order() {
        let (manager, events, dir) = recording("order");
        let added = manager.add(requests(3));
        let ids: Vec<String> = added.iter().map(|task| task.id.clone()).collect();
        start(&manager, &ids[0]);
        start(&manager, &ids[1]);
        manager.update(&ids[0], 0, |task| task.total_bytes = Some(10_000));
        manager.finish(&ids[0], 0, Ok(dir.join("Track 0.flac")));
        manager.cancel(&ids[2]).unwrap();
        manager.finish(&ids[1], 0, Err(failed("timed out")));
        let sent = take(&events);
        let truth = manager.snapshot();
        let _ = std::fs::remove_dir_all(&dir);

        // Revisions only grow, in the order things were sent.
        let mut last = 0;
        for event in &sent {
            let revision = match event {
                DownloadEvent::State(list) => list.revision,
                DownloadEvent::Changed(tasks) => {
                    tasks.iter().map(|task| task.revision).max().unwrap()
                }
            };
            assert!(revision > last, "{sent:?}");
            last = revision;
        }
        assert!(truth.revision >= last);

        assert_eq!(replay(&sent), truth.tasks);
        // The list first, then the changes the other way round: a late
        // change never overwrites a newer one.
        let (list, changes) = sent.split_first().unwrap();
        assert_eq!(
            replay(std::iter::once(list).chain(changes.iter().rev())),
            truth.tasks
        );
        // An answer to `get_downloads` from before the batch, arriving last,
        // is dropped as a whole.
        let stale = DownloadEvent::State(super::DownloadSnapshot {
            revision: 0,
            tasks: Vec::new(),
        });
        assert_eq!(replay(sent.iter().chain([&stale])), truth.tasks);
    }

    #[test]
    fn downloads_that_fail_report_through_changes_only() {
        let (manager, events, dir) = recording("run");
        manager.enqueue(requests(4));
        // Without a session every attempt fails at once — as far as the
        // events tell.
        let deadline = Instant::now() + Duration::from_secs(10);
        let all_failed = || {
            replay(events.lock().unwrap().iter())
                .iter()
                .filter(|task| task.status == DownloadStatus::Failed)
                .count()
                == 4
        };
        while !all_failed() {
            assert!(Instant::now() < deadline, "downloads did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        let sent = take(&events);
        let truth = manager.snapshot();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(sent[0], DownloadEvent::State(_)));
        assert!(sent[1..]
            .iter()
            .all(|event| matches!(event, DownloadEvent::Changed(_))));
        assert_eq!(replay(&sent), truth.tasks);
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
