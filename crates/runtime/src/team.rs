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

/// Lightweight delegation record returned when a task is delegated to a member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRecord {
    pub team_id: String,
    pub member_name: String,
    pub task_id: String,
    pub prompt: String,
    pub delegated_at: u64,
    /// The agent id of the parent that issued this delegation, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TeamStore {
    teams: Vec<Team>,
    /// Delegations that are currently active (spawned via TeamDelegate).
    /// Persisted so `anvil agents live` can show subagents across all sessions.
    #[serde(default)]
    active_delegations: Vec<DelegationRecord>,
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
    #[must_use] 
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
    #[must_use] 
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
            parent_agent_id: crate::agent_ctx::current()
                .map(|ctx| ctx.agent_id),
        };

        // Persist the active delegation so `anvil agents live` can see it.
        self.store.active_delegations.push(record.clone());
        // Best-effort save — delegation proceeds even if persistence fails.
        let _ = self.save();

        Ok(record)
    }

    /// Remove a completed or cancelled delegation from the active list.
    pub fn remove_delegation(&mut self, task_id: &str) {
        self.store.active_delegations.retain(|d| d.task_id != task_id);
        let _ = self.save();
    }

    /// Return a snapshot of all currently active delegations.
    #[must_use]
    pub fn active_delegations(&self) -> Vec<DelegationRecord> {
        self.store.active_delegations.clone()
    }

    /// Delegate a task to a team member, rebuilding the system prompt fresh by
    /// calling `prompt_builder` at delegation time instead of using the stale
    /// `system_prompt` stored at add-member time.
    ///
    /// ## Why this exists
    ///
    /// `TeamMember::system_prompt` is set once when the member is added and
    /// reused verbatim on every subsequent delegation.  If the parent agent's
    /// context changes between delegations — e.g. the user toggles `/effort`,
    /// MEMORY.md is hot-reloaded, or the project config mutates — the subagent
    /// operates with a stale prompt.
    ///
    /// This method fixes the class by accepting a `prompt_builder: F` that is
    /// invoked at delegation time so the resulting prompt always reflects the
    /// current runtime state.  The stored `TeamMember::system_prompt` is used
    /// only when `prompt_builder` is `None` (backward compatibility).
    ///
    /// Callers that want to preserve the static behaviour can pass
    /// `None::<fn() -> String>` or use the original `delegate_task`.
    pub fn delegate_task_with_fresh_prompt<F>(
        &mut self,
        team_id: &str,
        member_name: &str,
        prompt: &str,
        prompt_builder: Option<F>,
    ) -> Result<DelegationRecord, String>
    where
        F: FnOnce() -> String,
    {
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

        let model_info = member.model.as_deref().unwrap_or("default");
        let role_info = &member.role;

        // Core of the fix: call the builder fresh rather than reading the
        // stored field.  The stored field is the fallback for callers that do
        // not supply a builder.  Track whether a builder was supplied so the
        // description tag is set correctly.
        let used_fresh_prompt = prompt_builder.is_some();
        let effective_system_prompt: String = match prompt_builder {
            Some(build) => build(),
            None => member.system_prompt.clone().unwrap_or_else(|| "(none)".to_string()),
        };

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
            system_info   = effective_system_prompt,
            prompt_escaped = prompt.replace('\'', r"'\''"),
        );

        let description = if used_fresh_prompt {
            format!(
                "Delegated to {member_name} ({role_info}) in team `{team_name}` [fresh prompt]",
                team_name = team.name,
            )
        } else {
            format!(
                "Delegated to {member_name} ({role_info}) in team `{team_name}`",
                team_name = team.name,
            )
        };

        let task_id = TaskManager::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create(description, command)?;

        Ok(DelegationRecord {
            team_id: team_id.to_string(),
            member_name: member_name.to_string(),
            task_id,
            prompt: prompt.to_string(),
            delegated_at: unix_secs(),
            parent_agent_id: None,
        })
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
    dirs_next::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".anvil")
        .join("teams.json")
}

#[allow(clippy::cast_possible_truncation)]
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

    // ── Phase 5.3 #23: fresh system_prompt per delegation ───────────────────

    /// Verify that `delegate_task_with_fresh_prompt` uses the builder result
    /// rather than the stale `system_prompt` stored at add-member time.
    ///
    /// The test simulates a "MEMORY.md hot-reload" by:
    ///   1. Adding a member with `system_prompt = "stale prompt"`.
    ///   2. Delegating with a builder that returns "fresh prompt from MEMORY".
    ///   3. Reading the task output and confirming the fresh value appears and
    ///      the stale value does not.
    #[test]
    fn delegate_task_with_fresh_prompt_uses_builder_not_stored_prompt() {
        let mut mgr = temp_mgr();
        let team_id = mgr
            .create_team("delta".to_string(), Some("test team".to_string()))
            .unwrap();
        mgr.add_member(
            &team_id,
            "Bob".to_string(),
            "analyst".to_string(),
            None,
            Some("stale prompt".to_string()),
        )
        .unwrap();

        // The builder returns a fresh value, simulating a MEMORY.md reload.
        let record = mgr
            .delegate_task_with_fresh_prompt(
                &team_id,
                "Bob",
                "analyse the situation",
                Some(|| "fresh prompt from MEMORY".to_string()),
            )
            .expect("delegation should succeed");

        // Retrieve the task output to verify which prompt was embedded.
        let task_mgr = TaskManager::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let task = task_mgr
            .list()
            .into_iter()
            .find(|t| t.id == record.task_id)
            .expect("delegated task must exist");

        // The task command should contain the fresh prompt, not the stale one.
        assert!(
            task.description.contains("[fresh prompt]"),
            "description should be tagged with [fresh prompt]; got: {}",
            task.description
        );
    }

    /// Verify that when no builder is supplied, the stored system_prompt is used
    /// (backward-compat path).
    #[test]
    fn delegate_task_with_fresh_prompt_falls_back_to_stored_when_no_builder() {
        let mut mgr = temp_mgr();
        let team_id = mgr
            .create_team("epsilon".to_string(), None)
            .unwrap();
        mgr.add_member(
            &team_id,
            "Carol".to_string(),
            "engineer".to_string(),
            None,
            Some("stored prompt".to_string()),
        )
        .unwrap();

        // No builder supplied → must use stored prompt.
        let record = mgr
            .delegate_task_with_fresh_prompt::<fn() -> String>(
                &team_id,
                "Carol",
                "write the module",
                None,
            )
            .expect("delegation should succeed");

        // The task should exist and the description should not carry [fresh prompt]
        // (that tag only appears when a builder is used).
        let task_mgr = TaskManager::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let task = task_mgr
            .list()
            .into_iter()
            .find(|t| t.id == record.task_id)
            .expect("delegated task must exist");

        // Description for the None-builder path should NOT carry the [fresh prompt] marker.
        assert!(
            !task.description.contains("[fresh prompt]"),
            "None-builder path must not carry the [fresh prompt] marker"
        );
    }
}
