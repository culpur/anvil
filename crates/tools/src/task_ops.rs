use runtime::TaskManager;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::to_pretty_json;

// ---------------------------------------------------------------------------
// Task manager input types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct TaskCreateInput {
    pub(crate) description: String,
    pub(crate) command: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TaskGetInput {
    pub(crate) task_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TaskUpdateInput {
    pub(crate) task_id: String,
    pub(crate) status: Option<String>,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TaskOutputInput {
    pub(crate) task_id: String,
    pub(crate) block: Option<bool>,
    pub(crate) timeout: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TaskStopInput {
    pub(crate) task_id: String,
}

// ---------------------------------------------------------------------------
// TodoWrite input types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct TodoWriteInput {
    pub(crate) todos: Vec<TodoItem>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub(crate) struct TodoItem {
    pub(crate) content: String,
    /// Ollama and some other models omit `activeForm` or send `id` instead.
    #[serde(rename = "activeForm", default)]
    pub(crate) active_form: String,
    /// Allow models that send `id` instead of `activeForm`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    pub(crate) status: TodoStatus,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct TaskInfo {
    id: String,
    description: String,
    command: String,
    status: String,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Serialize)]
struct TodoWriteOutput {
    #[serde(rename = "oldTodos")]
    old_todos: Vec<TodoItem>,
    #[serde(rename = "newTodos")]
    new_todos: Vec<TodoItem>,
    #[serde(rename = "verificationNudgeNeeded")]
    verification_nudge_needed: Option<bool>,
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn task_info_from_manager(mgr: &TaskManager, id: &str) -> Option<TaskInfo> {
    mgr.get(id).map(|t| TaskInfo {
        id: t.id.clone(),
        description: t.description.clone(),
        command: t.command.clone(),
        status: t.status.as_str().to_string(),
        created_at: t.created_at,
        updated_at: t.updated_at,
    })
}

// ---------------------------------------------------------------------------
// Task runner functions
// ---------------------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_task_create(input: TaskCreateInput) -> Result<String, String> {
    let mut mgr = TaskManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let task_id = mgr.create(input.description, input.command)?;
    let info = task_info_from_manager(&mgr, &task_id)
        .ok_or_else(|| String::from("task vanished after creation"))?;
    to_pretty_json(info)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_task_get(input: TaskGetInput) -> Result<String, String> {
    let mgr = TaskManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let info = task_info_from_manager(&mgr, &input.task_id)
        .ok_or_else(|| format!("task `{}` not found", input.task_id))?;
    to_pretty_json(info)
}

pub(crate) fn run_task_list() -> Result<String, String> {
    let mgr = TaskManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let list: Vec<TaskInfo> = mgr
        .list()
        .iter()
        .map(|t| TaskInfo {
            id: t.id.clone(),
            description: t.description.clone(),
            command: t.command.clone(),
            status: t.status.as_str().to_string(),
            created_at: t.created_at,
            updated_at: t.updated_at,
        })
        .collect();
    to_pretty_json(list)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_task_update(input: TaskUpdateInput) -> Result<String, String> {
    let mut mgr = TaskManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mgr.update(
        &input.task_id,
        input.status.as_deref(),
        input.description.as_deref(),
    )?;
    let info = task_info_from_manager(&mgr, &input.task_id)
        .ok_or_else(|| format!("task `{}` not found after update", input.task_id))?;
    to_pretty_json(info)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_task_output(input: TaskOutputInput) -> Result<String, String> {
    let block = input.block.unwrap_or(true);
    let timeout_ms = input.timeout.unwrap_or(30_000);
    let timeout = std::time::Duration::from_millis(timeout_ms);

    let mut mgr = TaskManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let output = mgr.output(&input.task_id, block, timeout)?;
    to_pretty_json(json!({
        "task_id": input.task_id,
        "output": output,
    }))
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_task_stop(input: TaskStopInput) -> Result<String, String> {
    let mut mgr = TaskManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mgr.stop(&input.task_id)?;
    let info = task_info_from_manager(&mgr, &input.task_id)
        .ok_or_else(|| format!("task `{}` not found after stop", input.task_id))?;
    to_pretty_json(info)
}

// ---------------------------------------------------------------------------
// TodoWrite runner
// ---------------------------------------------------------------------------

pub(crate) fn run_todo_write(input: TodoWriteInput) -> Result<String, String> {
    to_pretty_json(execute_todo_write(input)?)
}

fn execute_todo_write(input: TodoWriteInput) -> Result<TodoWriteOutput, String> {
    validate_todos(&input.todos)?;
    let store_path = todo_store_path()?;
    let old_todos = if store_path.exists() {
        serde_json::from_str::<Vec<TodoItem>>(
            &std::fs::read_to_string(&store_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };

    let all_done = input
        .todos
        .iter()
        .all(|todo| matches!(todo.status, TodoStatus::Completed));
    let persisted = if all_done {
        Vec::new()
    } else {
        input.todos.clone()
    };

    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        &store_path,
        serde_json::to_string_pretty(&persisted).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let verification_nudge_needed = (all_done
        && input.todos.len() >= 3
        && !input
            .todos
            .iter()
            .any(|todo| todo.content.to_lowercase().contains("verif")))
    .then_some(true);

    Ok(TodoWriteOutput {
        old_todos,
        new_todos: input.todos,
        verification_nudge_needed,
    })
}

fn validate_todos(todos: &[TodoItem]) -> Result<(), String> {
    if todos.is_empty() {
        return Err(String::from("todos must not be empty"));
    }
    // Allow multiple in_progress items for parallel workflows
    if todos.iter().any(|todo| todo.content.trim().is_empty()) {
        return Err(String::from("todo content must not be empty"));
    }
    Ok(())
}

fn todo_store_path() -> Result<std::path::PathBuf, String> {
    if let Ok(path) = std::env::var("ANVIL_TODO_STORE") {
        return Ok(std::path::PathBuf::from(path));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    Ok(cwd.join(".anvil-todos.json"))
}
