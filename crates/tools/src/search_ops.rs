use runtime::{glob_search, grep_search, GrepSearchInput};
use serde::Deserialize;

use crate::{io_to_string, to_pretty_json};

#[derive(Debug, Deserialize)]
pub(crate) struct GlobSearchInputValue {
    pub(crate) pattern: String,
    pub(crate) path: Option<String>,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_glob_search(input: GlobSearchInputValue) -> Result<String, String> {
    to_pretty_json(glob_search(&input.pattern, input.path.as_deref()).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_grep_search(input: GrepSearchInput) -> Result<String, String> {
    to_pretty_json(grep_search(&input).map_err(io_to_string)?)
}
