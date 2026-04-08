use runtime::TeamManager;
use serde::Deserialize;

use crate::to_pretty_json;

#[derive(Debug, Deserialize)]
pub(crate) struct TeamCreateInput {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TeamAddMemberInput {
    pub(crate) team_id: String,
    pub(crate) name: String,
    pub(crate) role: String,
    pub(crate) model: Option<String>,
    pub(crate) system_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TeamRemoveMemberInput {
    pub(crate) team_id: String,
    pub(crate) member_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TeamDelegateInput {
    pub(crate) team_id: String,
    pub(crate) member_name: String,
    pub(crate) prompt: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TeamStatusInput {
    pub(crate) team_id: String,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_team_create(input: TeamCreateInput) -> Result<String, String> {
    let mut mgr = TeamManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let id = mgr.create_team(input.name, input.description)?;
    let team = mgr
        .get_team(&id)
        .ok_or_else(|| String::from("team vanished after creation"))?
        .clone();
    to_pretty_json(team)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_team_add_member(input: TeamAddMemberInput) -> Result<String, String> {
    let mut mgr = TeamManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mgr.add_member(
        &input.team_id,
        input.name,
        input.role,
        input.model,
        input.system_prompt,
    )?;
    let team = mgr
        .get_team(&input.team_id)
        .ok_or_else(|| format!("team `{}` not found after add", input.team_id))?
        .clone();
    to_pretty_json(team)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_team_remove_member(input: TeamRemoveMemberInput) -> Result<String, String> {
    let mut mgr = TeamManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mgr.remove_member(&input.team_id, &input.member_name)?;
    let team = mgr
        .get_team(&input.team_id)
        .ok_or_else(|| format!("team `{}` not found after remove", input.team_id))?
        .clone();
    to_pretty_json(team)
}

pub(crate) fn run_team_list() -> Result<String, String> {
    let mgr = TeamManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    to_pretty_json(mgr.list_teams())
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_team_delegate(input: TeamDelegateInput) -> Result<String, String> {
    let mut mgr = TeamManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let record = mgr.delegate_task(&input.team_id, &input.member_name, &input.prompt)?;
    to_pretty_json(record)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_team_status(input: TeamStatusInput) -> Result<String, String> {
    let mgr = TeamManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let status = mgr.get_status(&input.team_id)?;
    to_pretty_json(status)
}
