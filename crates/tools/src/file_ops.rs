use runtime::{edit_file, read_file, write_file};
use serde::Deserialize;

use crate::{io_to_string, to_pretty_json};

#[derive(Debug, Deserialize)]
pub(crate) struct ReadFileInput {
    pub(crate) path: String,
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WriteFileInput {
    pub(crate) path: String,
    pub(crate) content: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EditFileInput {
    pub(crate) path: String,
    pub(crate) old_string: String,
    pub(crate) new_string: String,
    pub(crate) replace_all: Option<bool>,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_read_file(input: ReadFileInput) -> Result<String, String> {
    to_pretty_json(read_file(&input.path, input.offset, input.limit).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_write_file(input: WriteFileInput) -> Result<String, String> {
    to_pretty_json(write_file(&input.path, &input.content).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_edit_file(input: EditFileInput) -> Result<String, String> {
    to_pretty_json(
        edit_file(
            &input.path,
            &input.old_string,
            &input.new_string,
            input.replace_all.unwrap_or(false),
        )
        .map_err(io_to_string)?,
    )
}
