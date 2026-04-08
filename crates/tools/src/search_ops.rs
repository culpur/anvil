use runtime::{glob_search, grep_search, GrepSearchInput};
use serde::Deserialize;

use crate::to_pretty_json;

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct GlobSearchInputValue {
    pub pattern: String,
    pub path: Option<String>,
}

// GrepSearchInput is re-exported from the runtime crate.
pub(crate) use runtime::GrepSearchInput as GrepInput;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_glob_search(input: GlobSearchInputValue) -> Result<String, String> {
    to_pretty_json(glob_search(&input.pattern, input.path.as_deref()).map_err(|e| e.to_string())?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_grep_search(input: GrepSearchInput) -> Result<String, String> {
    to_pretty_json(grep_search(&input).map_err(|e| e.to_string())?)
}
