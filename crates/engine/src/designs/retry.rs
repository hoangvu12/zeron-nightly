//! Retrying a failed call: its snippet is kept under an edit id, and the
//! agent fixes it by sending find-and-replace edits against that id, as
//! many times as it takes, instead of the whole snippet again.

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Most failed calls kept to retry; the oldest go first.
const MAX_KEPT: usize = 32;

/// Longest line of the snippet a failure quotes.
const MAX_CODE_CHARS: usize = 200;

const RETRY: &str = "Nothing was changed. To fix the snippet, call design_execute with this editId and \
    edits: [{find, replace}], each find being text that's in the snippet once; don't send the snippet again.";

/// One find-and-replace edit to a failed call's snippet.
#[derive(Debug, Deserialize)]
pub struct Edit {
    pub find: String,
    pub replace: String,
}

/// What a failed call returns: why it failed, where, and the edit id to
/// retry it with.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub error: String,
    /// The snippet's line it failed on, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// That line's code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub edit_id: String,
    pub retry: &'static str,
}

impl Failure {
    pub(super) fn new(error: String, line: Option<usize>, snippet: &str, edit_id: String) -> Self {
        let code = line
            .and_then(|line| snippet.lines().nth(line.checked_sub(1)?))
            .map(|code| code.trim().chars().take(MAX_CODE_CHARS).collect());
        Self {
            error,
            line,
            code,
            edit_id,
            retry: RETRY,
        }
    }
}

/// A failed call, kept for its chat to retry.
#[derive(Debug, Clone)]
pub(super) struct Kept {
    pub chat: Option<String>,
    /// The snippet as the latest try had it.
    pub snippet: String,
    pub description: Option<String>,
    pub target: Retarget,
}

/// The Design a failed call was for.
#[derive(Debug, Clone)]
pub(super) enum Retarget {
    Path(PathBuf),
    /// A new Design the call would have made.
    Name(String),
}

/// The failed calls kept, by edit id, oldest first.
#[derive(Debug, Default)]
pub(super) struct Failed {
    kept: VecDeque<(String, Kept)>,
}

impl Failed {
    /// Keeps a failed call under `edit_id` (a retry's), or a new id.
    /// Returns the id.
    pub fn keep(&mut self, edit_id: Option<String>, kept: Kept) -> String {
        let edit_id = edit_id.unwrap_or_else(new_edit_id);
        self.remove(&edit_id);
        self.kept.push_back((edit_id.clone(), kept));
        if self.kept.len() > MAX_KEPT {
            self.kept.pop_front();
        }
        edit_id
    }

    /// The failed call `edit_id`, if `chat` made it.
    pub fn get(&self, edit_id: &str, chat: Option<&str>) -> Option<&Kept> {
        self.kept
            .iter()
            .find(|(id, kept)| id == edit_id && kept.chat.as_deref() == chat)
            .map(|(_, kept)| kept)
    }

    pub fn remove(&mut self, edit_id: &str) {
        self.kept.retain(|(id, _)| id != edit_id);
    }
}

/// `snippet` with `edits` made in order, each one's `find` found exactly
/// once in the snippet as the edits before it left it.
pub(super) fn patched(snippet: &str, edits: &[Edit]) -> Result<String, String> {
    let mut snippet = snippet.to_owned();
    for (n, edit) in edits.iter().enumerate() {
        let n = n + 1;
        if edit.find.is_empty() {
            return Err(format!("edit {n}'s find is empty; nothing was changed"));
        }
        match snippet.matches(&edit.find).count() {
            1 => snippet = snippet.replacen(&edit.find, &edit.replace, 1),
            0 => {
                return Err(format!(
                    "edit {n}'s find isn't in the snippet: {:?}; nothing was changed",
                    edit.find
                ));
            }
            count => {
                return Err(format!(
                    "edit {n}'s find is in the snippet {count} times, so which to replace isn't clear: {:?}; \
                     make it longer. Nothing was changed",
                    edit.find
                ));
            }
        }
    }
    Ok(snippet)
}

fn new_edit_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..10].to_owned()
}
