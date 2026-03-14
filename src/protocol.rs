use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub struct WireEvent {
    pub repo: String,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub path: Option<String>,
    pub git_path: Option<String>,
    pub ts_unix_ms: u128,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    FileCreated,
    FileModified,
    FileDeleted,
    FileRenamed,
    DirCreated,
    DirDeleted,
    DirRenamed,
    RepoHeadChanged,
    RepoIndexChanged,
    RepoRefsChanged,
    RepoPackedRefsChanged,
    RepoChanged,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    AddWatch { repo: PathBuf },
    RemoveWatch { repo: PathBuf },
    ListWatches,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
}

impl ControlResponse {
    fn new(ok: bool) -> Self {
        Self {
            ok,
            message: None,
            error: None,
            repos: None,
        }
    }

    pub fn success_message(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            ..Self::new(true)
        }
    }

    pub fn list(repos: Vec<PathBuf>) -> Self {
        Self {
            repos: Some(
                repos
                    .into_iter()
                    .map(|repo| repo.display().to_string())
                    .collect(),
            ),
            ..Self::new(true)
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::new(false)
        }
    }
}
