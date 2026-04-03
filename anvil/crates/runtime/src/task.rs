use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Created,
    Running,
    Completed,
    Failed,
    Stopped,
}

impl TaskStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "created" => Some(Self::Created),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// A lightweight snapshot of a task that reached a terminal state.
#[derive(Debug, Clone)]
pub struct CompletedTaskInfo {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    /// Unix timestamp (seconds) when the terminal state was recorded.
    pub completed_at: u64,
}

/// A background shell task managed by `TaskManager`.
pub struct Task {
    pub id: String,
    pub description: String,
    pub command: String,
    pub status: TaskStatus,
    /// Captured stdout + stderr, shared with the reaper thread.
    pub output: Arc<Mutex<Vec<u8>>>,
    /// Set to `true` by `stop()` to ask the background thread to terminate.
    cancel: Arc<Mutex<bool>>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Set (in seconds since epoch) when the task first enters a terminal state.
    pub completed_at: Option<u64>,
    /// The `Instant` counterpart of `completed_at` for sub-second precision checks.
    pub completed_instant: Option<Instant>,
    /// Non-None while the child is still alive (before the reaper consumes it).
    child: Option<Arc<Mutex<Child>>>,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("id", &self.id)
            .field("description", &self.description)
            .field("command", &self.command)
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("completed_at", &self.completed_at)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// TaskManager
// ---------------------------------------------------------------------------

pub struct TaskManager {
    tasks: HashMap<String, Task>,
}

impl TaskManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    /// Return the process-global singleton.
    #[must_use]
    pub fn global() -> &'static Mutex<TaskManager> {
        static INSTANCE: OnceLock<Mutex<TaskManager>> = OnceLock::new();
        INSTANCE.get_or_init(|| Mutex::new(TaskManager::new()))
    }

    // -----------------------------------------------------------------------
    // CRUD
    // -----------------------------------------------------------------------

    /// Create a task and immediately spawn its command in a background thread.
    /// Returns the task ID on success.
    pub fn create(&mut self, description: String, command: String) -> Result<String, String> {
        let id = make_task_id();
        let now = unix_secs();
        let output_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let cancel_flag: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

        // Spawn the child process.
        let child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn command: {e}"))?;

        let child_arc: Arc<Mutex<Child>> = Arc::new(Mutex::new(child));

        let task = Task {
            id: id.clone(),
            description: description.clone(),
            command: command.clone(),
            status: TaskStatus::Running,
            output: Arc::clone(&output_buf),
            cancel: Arc::clone(&cancel_flag),
            created_at: now,
            updated_at: now,
            completed_at: None,
            completed_instant: None,
            child: Some(Arc::clone(&child_arc)),
        };
        self.tasks.insert(id.clone(), task);

        // Spawn a reaper thread that captures output and awaits exit.
        let task_id_clone = id.clone();
        let output_buf_clone = Arc::clone(&output_buf);
        let cancel_clone = Arc::clone(&cancel_flag);

        std::thread::Builder::new()
            .name(format!("anvil-task-{id}"))
            .spawn(move || {
                reap_task(
                    task_id_clone,
                    child_arc,
                    output_buf_clone,
                    cancel_clone,
                );
            })
            .map_err(|e| format!("failed to spawn reaper thread: {e}"))?;

        Ok(id)
    }

    /// Return a reference to a task by ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.get(id)
    }

    /// Return references to all tasks ordered by `created_at` ascending.
    #[must_use]
    pub fn list(&self) -> Vec<&Task> {
        let mut tasks: Vec<&Task> = self.tasks.values().collect();
        tasks.sort_by_key(|t| t.created_at);
        tasks
    }

    /// Update task metadata.  Accepts optional new status (as string) and
    /// optional new description.  Status transitions are validated.
    pub fn update(
        &mut self,
        id: &str,
        new_status: Option<&str>,
        new_description: Option<&str>,
    ) -> Result<(), String> {
        let task = self.tasks.get_mut(id).ok_or_else(|| format!("task `{id}` not found"))?;
        if let Some(s) = new_status {
            task.status =
                TaskStatus::from_str(s).ok_or_else(|| format!("unknown status `{s}`"))?;
        }
        if let Some(d) = new_description {
            task.description = d.to_string();
        }
        task.updated_at = unix_secs();
        Ok(())
    }

    /// Read captured output for a task.
    ///
    /// `block` — if true, wait up to `timeout` for the task to finish before
    ///           returning the final buffer.  If false, return whatever has
    ///           been captured so far.
    pub fn output(
        &mut self,
        id: &str,
        block: bool,
        timeout: Duration,
    ) -> Result<String, String> {
        if !self.tasks.contains_key(id) {
            return Err(format!("task `{id}` not found"));
        }

        if block {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                // Re-borrow each iteration to check status.
                let status = &self.tasks[id].status;
                let done = matches!(
                    status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped
                );
                if done || std::time::Instant::now() >= deadline {
                    break;
                }
                // Release the lock briefly to let the reaper thread make progress.
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        let task = &self.tasks[id];
        let bytes = task
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Signal the background thread to stop the task.
    pub fn stop(&mut self, id: &str) -> Result<(), String> {
        let task = self.tasks.get_mut(id).ok_or_else(|| format!("task `{id}` not found"))?;
        // Set the cancel flag — the reaper thread will kill the child.
        *task
            .cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        task.status = TaskStatus::Stopped;
        task.updated_at = unix_secs();
        Ok(())
    }

    /// Return snapshots of tasks that entered a terminal state after `since`.
    /// The check uses a monotonic `Instant` so it is not affected by clock
    /// skew or system time changes.
    #[must_use]
    pub fn completed_since(&self, since: Instant) -> Vec<CompletedTaskInfo> {
        self.tasks
            .values()
            .filter_map(|task| {
                let instant = task.completed_instant?;
                if instant > since {
                    Some(CompletedTaskInfo {
                        id: task.id.clone(),
                        description: task.description.clone(),
                        status: task.status.clone(),
                        completed_at: task.completed_at.unwrap_or_default(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Internal: called by the reaper thread to update terminal state
    // -----------------------------------------------------------------------

    fn set_terminal_status(&mut self, id: &str, status: TaskStatus) {
        if let Some(task) = self.tasks.get_mut(id) {
            // Only transition from Running; Stopped stays Stopped.
            if task.status == TaskStatus::Running {
                let now_secs = unix_secs();
                let now_instant = Instant::now();
                task.status = status;
                task.completed_at = Some(now_secs);
                task.completed_instant = Some(now_instant);
            }
            task.updated_at = unix_secs();
            task.child = None;
        }
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Background reaper
// ---------------------------------------------------------------------------

fn reap_task(
    task_id: String,
    child_arc: Arc<Mutex<Child>>,
    output_buf: Arc<Mutex<Vec<u8>>>,
    cancel: Arc<Mutex<bool>>,
) {
    // Drain stdout and stderr by taking them out of the child.
    let (mut stdout_pipe, mut stderr_pipe) = {
        let mut child = child_arc.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (child.stdout.take(), child.stderr.take())
    };

    // Stream stdout and stderr into the shared buffer until EOF or cancel.
    let mut tmp = [0u8; 4096];

    loop {
        let cancelled = *cancel.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if cancelled {
            // Kill the child process.
            let mut child = child_arc.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = child.kill();
            break;
        }

        let mut any_data = false;

        if let Some(ref mut out) = stdout_pipe {
            match out.read(&mut tmp) {
                Ok(0) => {
                    // EOF — drop the pipe
                    stdout_pipe = None;
                }
                Ok(n) => {
                    let mut buf =
                        output_buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    buf.extend_from_slice(&tmp[..n]);
                    any_data = true;
                }
                Err(_) => {
                    stdout_pipe = None;
                }
            }
        }

        if let Some(ref mut err) = stderr_pipe {
            match err.read(&mut tmp) {
                Ok(0) => {
                    stderr_pipe = None;
                }
                Ok(n) => {
                    let mut buf =
                        output_buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    buf.extend_from_slice(&tmp[..n]);
                    any_data = true;
                }
                Err(_) => {
                    stderr_pipe = None;
                }
            }
        }

        // Both pipes gone — process has exited.
        if stdout_pipe.is_none() && stderr_pipe.is_none() {
            break;
        }

        if !any_data {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // Wait for the child to fully exit and determine exit status.
    let exit_ok = {
        let mut child = child_arc.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        child
            .wait()
            .map(|status| status.success())
            .unwrap_or(false)
    };

    // Update the TaskManager with the terminal status.
    let final_status = if *cancel.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
        TaskStatus::Stopped
    } else if exit_ok {
        TaskStatus::Completed
    } else {
        TaskStatus::Failed
    };

    if let Ok(mut mgr) = TaskManager::global().lock() {
        mgr.set_terminal_status(&task_id, final_status);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // 8-char alphanumeric derived from time.
    let raw = secs.wrapping_mul(1_000_000_007).wrapping_add(u64::from(nanos));
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789".chars().collect();
    let base = chars.len() as u64;
    let mut n = raw;
    let mut result = String::with_capacity(8);
    for _ in 0..8 {
        result.push(chars[(n % base) as usize]);
        n /= base;
    }
    result
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
