// goals.rs — stub (will be replaced by W3 stash)
// Minimal types to allow the codebase to compile before W3 is applied.
// This stub implements the full API surface that main.rs calls.

/// Maximum number of characters in a goal description.
pub const GOAL_DESCRIPTION_MAX: usize = 4096;
/// Truncation length for goal description in list view.
pub const GOAL_LIST_DESCRIPTION_TRUNCATE: usize = 80;
/// Truncation length for status-line display.
pub const GOAL_STATUS_LINE_TRUNCATE: usize = 60;

/// Status of a persistent goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalStatus {
    Active,
    Paused,
    Done,
}

impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Paused => write!(f, "paused"),
            Self::Done => write!(f, "done"),
        }
    }
}

/// A single persistent goal.
#[derive(Debug, Clone)]
pub struct Goal {
    pub id: String,
    pub description: String,
    pub status: GoalStatus,
}

/// Errors from goal operations.
#[derive(Debug)]
pub enum GoalError {
    Io(std::io::Error),
    NotFound(String),
    GoalNotFound(String),
    NoActiveGoal,
    DescriptionTooLong { len: usize, max: usize },
}

impl std::fmt::Display for GoalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "goal I/O error: {e}"),
            Self::NotFound(id) | Self::GoalNotFound(id) => write!(f, "goal not found: {id}"),
            Self::NoActiveGoal => write!(f, "no active goal"),
            Self::DescriptionTooLong { len, max } => {
                write!(f, "description too long ({len} > {max})")
            }
        }
    }
}

/// Manages goals for a project directory.
pub struct GoalManager {
    #[allow(dead_code)]
    goals_dir: std::path::PathBuf,
}

impl GoalManager {
    #[must_use]
    pub fn new(goals_dir: std::path::PathBuf) -> Self {
        Self { goals_dir }
    }

    pub fn list(&self) -> Result<Vec<Goal>, GoalError> {
        Ok(vec![])
    }

    pub fn create(&mut self, description: &str) -> Result<Goal, GoalError> {
        self.new_goal(description)
    }

    pub fn new_goal(&mut self, description: &str) -> Result<Goal, GoalError> {
        if description.len() > GOAL_DESCRIPTION_MAX {
            return Err(GoalError::DescriptionTooLong {
                len: description.len(),
                max: GOAL_DESCRIPTION_MAX,
            });
        }
        let id = format!("{:x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis());
        Ok(Goal {
            id,
            description: description.to_owned(),
            status: GoalStatus::Active,
        })
    }

    pub fn set_status(&mut self, id: &str, status: GoalStatus) -> Result<(), GoalError> {
        let _ = (id, status);
        Ok(())
    }

    pub fn resume(&mut self, id: &str) -> Result<Goal, GoalError> {
        Err(GoalError::GoalNotFound(id.to_owned()))
    }

    pub fn pause(&mut self, id: Option<&str>) -> Result<Goal, GoalError> {
        let _ = id;
        Err(GoalError::NoActiveGoal)
    }

    pub fn done(&mut self, id: Option<&str>) -> Result<Goal, GoalError> {
        let _ = id;
        Err(GoalError::NoActiveGoal)
    }

    pub fn get(&self, id: &str) -> Option<Goal> {
        let _ = id;
        None
    }

    pub fn active_goal(&self) -> Option<Goal> {
        None
    }
}

/// Format a list of goals for display.
#[must_use]
pub fn format_goal_list(goals: &[Goal]) -> String {
    if goals.is_empty() {
        return "No goals yet. Use /goal new \"<description>\" to create one.".into();
    }
    let mut out = String::new();
    for g in goals {
        out.push_str(&format!("[{}] {} — {}\n", g.status, g.id, g.description));
    }
    out
}

/// Format a single goal for show.
#[must_use]
pub fn format_goal_show(goal: &Goal) -> String {
    format!("Goal {}\nStatus: {}\nDescription: {}", goal.id, goal.status, goal.description)
}

/// Build a system-prompt fragment for the active goal.
#[must_use]
pub fn build_active_goal_prompt_fragment(goal: &Goal) -> String {
    format!("Active goal: {}", goal.description)
}
