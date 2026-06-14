use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub struct WireEvent {
    pub folder: String,
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
    DirModified,
    DirDeleted,
    DirRenamed,
    GitHeadChanged,
    GitIndexChanged,
    GitRefsChanged,
    GitPackedRefsChanged,
    GitChanged,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    AddWatch { folder: PathBuf },
    RemoveWatch { folder: PathBuf },
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
    pub folders: Option<Vec<String>>,
}

impl ControlResponse {
    fn new(ok: bool) -> Self {
        Self {
            ok,
            message: None,
            error: None,
            folders: None,
        }
    }

    pub fn success_message(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            ..Self::new(true)
        }
    }

    pub fn list(folders: Vec<PathBuf>) -> Self {
        Self {
            folders: Some(
                folders
                    .into_iter()
                    .map(|folder| folder.display().to_string())
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
