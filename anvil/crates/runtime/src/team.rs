use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::task::TaskManager;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMember {
    pub name: String,
    pub role: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub members: Vec<TeamMember>,
    /// Key-value store for shared context visible to all members.
    pub shared_context: HashMap<String, String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TeamStore {
    teams: Vec<Team>,
}

/// Lightweight delegation record returned when a task is delegated to a member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRecord {
    pub team_id: String,
    pub member_name: String,
    pub task_id: String,
    pub prompt: String,
    pub delegated_at: u64,
}

// ---------------------------------------------------------------------------
// TeamManager
// ---------------------------------------------------------------------------

pub struct TeamManager {
    store: TeamStore,
    store_path: PathBuf,
}

impl TeamManager {
    /// Construct by loading (or creating) the persistent store.
    pub fn new(store_path: PathBuf) -> Self {
        let store = Self::load_store(&store_path);
        Self { store, store_path }
    }

    /// Return the process-global singleton, persisted at `~/.anvil/teams.json`.
    #[must_use]
    pub fn global() -> &'static Mutex<TeamManager> {
        static INSTANCE: OnceLock<Mutex<TeamManager>> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let path = default_store_path();
            Mutex::new(TeamManager::new(path))
        })
    }

    // -----------------------------------------------------------------------
    // Team CRUD
    // -----------------------------------------------------------------------

    /// Create a new team.  Returns the team ID.
    pub fn create_team(&mut self, name: String, description: Option<String>) -> Result<String, String> {
        // Reject duplicate names to avoid confusion.
        if self.store.teams.iter().any(|t| t.name == name) {
            return Err(format!("a team named `{name}` already exists"));
        }
        let id = make_id();
        let now = unix_secs();
        self.store.teams.push(Team {
            id: id.clone(),
            name,
            description,
            members: Vec::new(),
            shared_context: HashMap::new(),
            created_at: now,
            updated_at: now,
        });
        self.save()?;
        Ok(id)
    }

    /// Return a snapshot of all teams ordered by `created_at`.
    #[must_use]
    pub fn list_teams(&self) -> Vec<Team> {
        let mut teams = self.store.teams.clone();
        teams.sort_by_key(|t| t.created_at);
        teams
    }

    /// Return a reference to a team by ID.
    pub fn get_team(&self, team_id: &str) -> Option<&Team> {
        self.store.teams.iter().find(|t| t.id == team_id)
    }

    // -----------------------------------------------------------------------
    // Member management
    // -----------------------------------------------------------------------

    /// Add a member to a team.  Returns an error if the name is already taken
    /// within the team.
    pub fn add_member(
        &mut self,
        team_id: &str,
        name: String,
        role: String,
        model: Option<String>,
        system_prompt: Option<String>,
    ) -> Result<(), String> {
        let team = self
            .store
            .teams
            .iter_mut()
            .find(|t| t.id == team_id)
            .ok_or_else(|| format!("team `{team_id}` not found"))?;

        if team.members.iter().any(|m| m.name == name) {
            return Err(format!("team `{team_id}` already has a member named `{name}`"));
        }

        team.members.push(TeamMember { name, role, model, system_prompt });
        team.updated_at = unix_secs();
        self.save()
    }

    /// Remove a member from a team by name.
    pub fn remove_member(&mut self, team_id: &str, member_name: &str) -> Result<(), String> {
        let team = self
            .store
            .teams
            .iter_mut()
            .find(|t| t.id == team_id)
            .ok_or_else(|| format!("team `{team_id}` not found"))?;

        let before = team.members.len();
        team.members.retain(|m| m.name != member_name);
        if team.members.len() == before {
            return Err(format!(
                "team `{team_id}` has no member named `{member_name}`"
            ));
        }
        team.updated_at = unix_secs();
        self.save()
    }

    // -----------------------------------------------------------------------
    // Delegation
    // -----------------------------------------------------------------------

    /// Delegate a task to a specific team member by creating a background
    /// `TaskManager` shell task that echoes the prompt and member context.
    /// Returns a `DelegationRecord` with the resulting task ID.
    pub fn delegate_task(
        &mut self,
        team_id: &str,
        member_name: &str,
        prompt: &str,
    ) -> Result<DelegationRecord, String> {
        let team = self
            .store
            .teams
            .iter()
            .find(|t| t.id == team_id)
            .ok_or_else(|| format!("team `{team_id}` not found"))?;

        let member = team
            .members
            .iter()
            .find(|m| m.name == member_name)
            .ok_or_else(|| format!("no member named `{member_name}` in team `{team_id}`"))?
            .clone();

        // Build a shell command that records the delegation details.  In a
        // production system this would invoke the AI runtime; here we store
        // the delegation as a structured log so callers can retrieve it via
        // TaskOutput.
        let model_info = member.model.as_deref().unwrap_or("default");
        let role_info = &member.role;
        let system_info = member
            .system_prompt
            .as_deref()
            .unwrap_or("(none)");

        let command = format!(
            r#"printf '%s\n' \
  "=== Team Delegation ===" \
  "Team:   {team_name}" \
  "Member: {member_name}" \
  "Role:   {role_info}" \
  "Model:  {model_info}" \
  "System: {system_info}" \
  "" \
  "Prompt:" \
  "{prompt_escaped}""#,
            team_name     = team.name,
            member_name   = member_name,
            role_info     = role_info,
            model_info    = model_info,
            system_info   = system_info,
            prompt_escaped = prompt.replace('\'', r"'\''"),
        );

        let description = format!(
            "Delegated to {member_name} ({role_info}) in team `{team_name}`",
            team_name = team.name,
        );

        let task_id = TaskManager::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create(description, command)?;

        let record = DelegationRecord {
            team_id: team_id.to_string(),
            member_name: member_name.to_string(),
            task_id,
            prompt: prompt.to_string(),
            delegated_at: unix_secs(),
        };

        Ok(record)
    }

    // -----------------------------------------------------------------------
    // Status
    // -----------------------------------------------------------------------

    /// Return a rich status snapshot for a team including member task states.
    pub fn get_status(&self, team_id: &str) -> Result<TeamStatus, String> {
        let team = self
            .store
            .teams
            .iter()
            .find(|t| t.id == team_id)
            .ok_or_else(|| format!("team `{team_id}` not found"))?;

        let task_mgr = TaskManager::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let members: Vec<MemberStatus> = team
            .members
            .iter()
            .map(|m| {
                // Find the most recent task whose description mentions this member.
                let latest_task = task_mgr
                    .list()
                    .into_iter()
                    .rev()
                    .find(|t| t.description.contains(m.name.as_str()))
                    .map(|t| TaskSnapshot {
                        task_id: t.id.clone(),
                        status: t.status.as_str().to_string(),
                        updated_at: t.updated_at,
                    });

                MemberStatus {
                    name: m.name.clone(),
                    role: m.role.clone(),
                    model: m.model.clone(),
                    latest_task,
                }
            })
            .collect();

        Ok(TeamStatus {
            team_id: team.id.clone(),
            team_name: team.name.clone(),
            description: team.description.clone(),
            member_count: team.members.len(),
            members,
            created_at: team.created_at,
            updated_at: team.updated_at,
        })
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    fn load_store(path: &PathBuf) -> TeamStore {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => TeamStore::default(),
        }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.store_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create team dir: {e}"))?;
        }
        let json =
            serde_json::to_string_pretty(&self.store).map_err(|e| format!("serialize error: {e}"))?;
        fs::write(&self.store_path, json)
            .map_err(|e| format!("cannot write team store: {e}"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Status types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    pub status: String,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberStatus {
    pub name: String,
    pub role: String,
    pub model: Option<String>,
    pub latest_task: Option<TaskSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamStatus {
    pub team_id: String,
    pub team_name: String,
    pub description: Option<String>,
    pub member_count: usize,
    pub members: Vec<MemberStatus>,
    pub created_at: u64,
    pub updated_at: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".anvil").join("teams.json")
}

fn make_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let secs = unix_secs();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_mgr() -> TeamManager {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("anvil-test-teams-{}-{n}.json", unix_secs()));
        TeamManager::new(tmp)
    }

    #[test]
    fn create_and_list_team() {
        let mut mgr = temp_mgr();
        let id = mgr.create_team("alpha".to_string(), None).unwrap();
        let teams = mgr.list_teams();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].id, id);
        assert_eq!(teams[0].name, "alpha");
    }

    #[test]
    fn duplicate_team_name_rejected() {
        let mut mgr = temp_mgr();
        mgr.create_team("alpha".to_string(), None).unwrap();
        assert!(mgr.create_team("alpha".to_string(), None).is_err());
    }

    #[test]
    fn add_and_remove_member() {
        let mut mgr = temp_mgr();
        let team_id = mgr.create_team("beta".to_string(), None).unwrap();
        mgr.add_member(&team_id, "Alice".to_string(), "engineer".to_string(), None, None)
            .unwrap();
        let team = mgr.get_team(&team_id).unwrap();
        assert_eq!(team.members.len(), 1);
        assert_eq!(team.members[0].name, "Alice");

        mgr.remove_member(&team_id, "Alice").unwrap();
        let team = mgr.get_team(&team_id).unwrap();
        assert!(team.members.is_empty());
    }

    #[test]
    fn remove_nonexistent_member_errors() {
        let mut mgr = temp_mgr();
        let id = mgr.create_team("gamma".to_string(), None).unwrap();
        assert!(mgr.remove_member(&id, "Nobody").is_err());
    }
}
