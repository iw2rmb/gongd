use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use notify::{
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode},
    Event, EventKind,
};
use tokio::sync::Mutex;

use crate::{
    protocol::{EventType, WireEvent},
    repo::RepoState,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DedupKey {
    repo: PathBuf,
    event_type: EventType,
    path: Option<PathBuf>,
    git_path: Option<String>,
}

#[derive(Debug)]
pub struct Deduper {
    window: Duration,
    seen: Vec<(DedupKey, Instant)>,
}

pub type SharedDeduper = Arc<Mutex<Deduper>>;

impl Deduper {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            seen: Vec::new(),
        }
    }

    fn should_emit(&mut self, key: DedupKey) -> bool {
        let now = Instant::now();
        self.seen
            .retain(|(_, seen_at)| now.duration_since(*seen_at) <= self.window);
        if self.seen.iter().any(|(seen_key, _)| *seen_key == key) {
            return false;
        }
        self.seen.push((key, now));
        true
    }
}

pub async fn translate_event(
    repos: &[RepoState],
    event: Event,
    deduper: SharedDeduper,
) -> Vec<WireEvent> {
    let mut out = Vec::new();

    for path in &event.paths {
        if let Some(repo) = repos.iter().find(|repo| path.starts_with(&repo.root)) {
            if repo.is_inside_git_dir(path) {
                if let Some(event) =
                    translate_git_event(repo, path, &event.kind, deduper.clone()).await
                {
                    out.push(event);
                }
            } else if let Some(event) =
                translate_worktree_event(repo, path, &event.kind, deduper.clone()).await
            {
                out.push(event);
            }
        }
    }

    if matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            | EventKind::Modify(ModifyKind::Name(_))
    ) && event.paths.len() >= 2
    {
        let from = &event.paths[0];
        let to = &event.paths[1];
        if let Some(repo) = repos
            .iter()
            .find(|repo| from.starts_with(&repo.root) || to.starts_with(&repo.root))
        {
            if !repo.is_inside_git_dir(from) && !repo.is_inside_git_dir(to) {
                if let Some(event) = emit_rename_event(repo, from, to, deduper).await {
                    out.push(event);
                }
            }
        }
    }

    out
}

async fn translate_worktree_event(
    repo: &RepoState,
    path: &Path,
    kind: &EventKind,
    deduper: SharedDeduper,
) -> Option<WireEvent> {
    let rel = path.strip_prefix(&repo.root).ok()?;
    let path_is_dir = path.is_dir();
    if repo.is_worktree_ignored(rel, path_is_dir) {
        return None;
    }

    let event_type = match kind {
        EventKind::Create(CreateKind::Folder) => EventType::DirCreated,
        EventKind::Create(_) => EventType::FileCreated,
        EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Metadata(_))
        | EventKind::Modify(ModifyKind::Any)
        | EventKind::Access(_) => {
            if path_is_dir {
                EventType::DirCreated
            } else {
                EventType::FileModified
            }
        }
        EventKind::Remove(RemoveKind::Folder) => EventType::DirDeleted,
        EventKind::Remove(_) => {
            if path_is_dir {
                EventType::DirDeleted
            } else {
                EventType::FileDeleted
            }
        }
        _ => return None,
    };

    let key = DedupKey {
        repo: repo.root.clone(),
        event_type,
        path: Some(rel.to_path_buf()),
        git_path: None,
    };

    let mut deduper = deduper.lock().await;
    if !deduper.should_emit(key) {
        return None;
    }

    Some(WireEvent {
        repo: repo.root.display().to_string(),
        event_type,
        path: Some(rel.to_string_lossy().into_owned()),
        git_path: None,
        ts_unix_ms: now_ms(),
    })
}

async fn emit_rename_event(
    repo: &RepoState,
    from: &Path,
    to: &Path,
    deduper: SharedDeduper,
) -> Option<WireEvent> {
    let rel_from = from.strip_prefix(&repo.root).ok()?;
    let rel_to = to.strip_prefix(&repo.root).ok()?;

    if repo.is_worktree_ignored(rel_from, from.is_dir())
        && repo.is_worktree_ignored(rel_to, to.is_dir())
    {
        return None;
    }

    let event_type = if from.is_dir() || to.is_dir() {
        EventType::DirRenamed
    } else {
        EventType::FileRenamed
    };

    let key = DedupKey {
        repo: repo.root.clone(),
        event_type,
        path: Some(rel_to.to_path_buf()),
        git_path: None,
    };

    let mut deduper = deduper.lock().await;
    if !deduper.should_emit(key) {
        return None;
    }

    Some(WireEvent {
        repo: repo.root.display().to_string(),
        event_type,
        path: Some(format!(
            "{} -> {}",
            rel_from.to_string_lossy(),
            rel_to.to_string_lossy()
        )),
        git_path: None,
        ts_unix_ms: now_ms(),
    })
}

async fn translate_git_event(
    repo: &RepoState,
    path: &Path,
    kind: &EventKind,
    deduper: SharedDeduper,
) -> Option<WireEvent> {
    let rel = path.strip_prefix(&repo.git_dir).ok()?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    let event_type = match rel_str.as_str() {
        "HEAD" => EventType::RepoHeadChanged,
        "index" => EventType::RepoIndexChanged,
        "packed-refs" => EventType::RepoPackedRefsChanged,
        value if value.starts_with("refs/") => EventType::RepoRefsChanged,
        _ => EventType::RepoChanged,
    };

    match kind {
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {}
        _ => return None,
    }

    let key = DedupKey {
        repo: repo.root.clone(),
        event_type,
        path: None,
        git_path: Some(rel_str.clone()),
    };

    let mut deduper = deduper.lock().await;
    if !deduper.should_emit(key) {
        return None;
    }

    Some(WireEvent {
        repo: repo.root.display().to_string(),
        event_type,
        path: None,
        git_path: Some(rel_str),
        ts_unix_ms: now_ms(),
    })
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
