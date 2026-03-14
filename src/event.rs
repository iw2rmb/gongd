use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use notify::{
    event::{CreateKind, ModifyKind, RemoveKind},
    Event, EventKind,
};
use tokio::sync::Mutex;

use crate::{
    protocol::{EventType, WireEvent},
    repo::RepoState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Dir,
}

impl EntryKind {
    fn is_dir(self) -> bool {
        matches!(self, Self::Dir)
    }

    fn created_event_type(self) -> EventType {
        match self {
            Self::File => EventType::FileCreated,
            Self::Dir => EventType::DirCreated,
        }
    }

    fn deleted_event_type(self) -> EventType {
        match self {
            Self::File => EventType::FileDeleted,
            Self::Dir => EventType::DirDeleted,
        }
    }

    fn modified_event_type(self) -> EventType {
        match self {
            Self::File => EventType::FileModified,
            Self::Dir => EventType::DirCreated,
        }
    }
}

#[derive(Debug)]
struct PendingEvent {
    event_type: EventType,
    path: Option<String>,
    git_path: Option<String>,
    dedup_path: Option<PathBuf>,
    dedup_git_path: Option<String>,
}

impl PendingEvent {
    fn worktree(event_type: EventType, rel: &Path) -> Self {
        let rel_path = rel.to_path_buf();
        Self {
            event_type,
            path: Some(rel.to_string_lossy().into_owned()),
            git_path: None,
            dedup_path: Some(rel_path),
            dedup_git_path: None,
        }
    }

    fn rename(event_type: EventType, from: &Path, to: &Path) -> Self {
        Self {
            event_type,
            path: Some(format!(
                "{} -> {}",
                from.to_string_lossy(),
                to.to_string_lossy()
            )),
            git_path: None,
            dedup_path: Some(to.to_path_buf()),
            dedup_git_path: None,
        }
    }

    fn git(event_type: EventType, rel: String) -> Self {
        Self {
            event_type,
            path: None,
            git_path: Some(rel.clone()),
            dedup_path: None,
            dedup_git_path: Some(rel),
        }
    }
}

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
        if let Some(event) = translate_path_event(repos, path, event.kind, &deduper).await {
            out.push(event);
        }
    }

    if is_rename_event(event.kind) && event.paths.len() >= 2 {
        let from = &event.paths[0];
        let to = &event.paths[1];
        if let Some(repo) = repo_for_paths(repos, from, to) {
            if !repo.is_inside_git_dir(from) && !repo.is_inside_git_dir(to) {
                if let Some(event) = emit_rename_event(repo, from, to, &deduper).await {
                    out.push(event);
                }
            }
        }
    }

    out
}

async fn translate_path_event(
    repos: &[RepoState],
    path: &Path,
    kind: EventKind,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let repo = repo_for_path(repos, path)?;
    if repo.is_inside_git_dir(path) {
        translate_git_event(repo, path, kind, deduper).await
    } else {
        translate_worktree_event(repo, path, kind, deduper).await
    }
}

async fn translate_worktree_event(
    repo: &RepoState,
    path: &Path,
    kind: EventKind,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let rel = path.strip_prefix(&repo.root).ok()?;
    let entry_kind = worktree_entry_kind(kind, path)?;
    if repo.is_worktree_ignored(rel, entry_kind.is_dir()) {
        return None;
    }

    emit_deduped_event(
        repo,
        PendingEvent::worktree(worktree_event_type(kind, entry_kind)?, rel),
        deduper,
    )
    .await
}

async fn emit_rename_event(
    repo: &RepoState,
    from: &Path,
    to: &Path,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let rel_from = from.strip_prefix(&repo.root).ok()?;
    let rel_to = to.strip_prefix(&repo.root).ok()?;
    let entry_kind = rename_entry_kind(from, to);

    if repo.is_worktree_ignored(rel_from, entry_kind.is_dir())
        && repo.is_worktree_ignored(rel_to, entry_kind.is_dir())
    {
        return None;
    }

    let event_type = match entry_kind {
        EntryKind::Dir => EventType::DirRenamed,
        EntryKind::File => EventType::FileRenamed,
    };

    emit_deduped_event(
        repo,
        PendingEvent::rename(event_type, rel_from, rel_to),
        deduper,
    )
    .await
}

async fn translate_git_event(
    repo: &RepoState,
    path: &Path,
    kind: EventKind,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let rel = path.strip_prefix(&repo.git_dir).ok()?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let event_type = git_event_type(&rel_str);

    match kind {
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {}
        _ => return None,
    }

    emit_deduped_event(repo, PendingEvent::git(event_type, rel_str), deduper).await
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn repo_for_path<'a>(repos: &'a [RepoState], path: &Path) -> Option<&'a RepoState> {
    repos.iter().find(|repo| path.starts_with(&repo.root))
}

fn repo_for_paths<'a>(repos: &'a [RepoState], left: &Path, right: &Path) -> Option<&'a RepoState> {
    repos
        .iter()
        .find(|repo| left.starts_with(&repo.root) && right.starts_with(&repo.root))
}

fn is_rename_event(kind: EventKind) -> bool {
    matches!(kind, EventKind::Modify(ModifyKind::Name(_)))
}

fn worktree_entry_kind(kind: EventKind, path: &Path) -> Option<EntryKind> {
    match kind {
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => {
            Some(EntryKind::Dir)
        }
        EventKind::Create(CreateKind::File) | EventKind::Remove(RemoveKind::File) => {
            Some(EntryKind::File)
        }
        EventKind::Create(_)
        | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any)
        | EventKind::Access(_)
        | EventKind::Remove(_) => Some(entry_kind_from_path(path)),
        _ => None,
    }
}

fn rename_entry_kind(from: &Path, to: &Path) -> EntryKind {
    if to.is_dir() || from.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

fn entry_kind_from_path(path: &Path) -> EntryKind {
    if path.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

fn worktree_event_type(kind: EventKind, entry_kind: EntryKind) -> Option<EventType> {
    match kind {
        EventKind::Create(_) => Some(entry_kind.created_event_type()),
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any)
        | EventKind::Access(_) => Some(entry_kind.modified_event_type()),
        EventKind::Remove(_) => Some(entry_kind.deleted_event_type()),
        _ => None,
    }
}

fn git_event_type(rel_str: &str) -> EventType {
    match rel_str {
        "HEAD" => EventType::RepoHeadChanged,
        "index" => EventType::RepoIndexChanged,
        "packed-refs" => EventType::RepoPackedRefsChanged,
        value if value.starts_with("refs/") => EventType::RepoRefsChanged,
        _ => EventType::RepoChanged,
    }
}

async fn emit_deduped_event(
    repo: &RepoState,
    event: PendingEvent,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let key = DedupKey {
        repo: repo.root.clone(),
        event_type: event.event_type,
        path: event.dedup_path,
        git_path: event.dedup_git_path,
    };

    let mut deduper = deduper.lock().await;
    if !deduper.should_emit(key) {
        return None;
    }

    Some(WireEvent {
        repo: repo.root.display().to_string(),
        event_type: event.event_type,
        path: event.path,
        git_path: event.git_path,
        ts_unix_ms: now_ms(),
    })
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use notify::{
        event::{EventAttributes, RemoveKind},
        Event, EventKind,
    };
    use tokio::sync::Mutex;

    use super::{translate_event, Deduper, SharedDeduper};
    use crate::{
        repo::RepoState,
        test_support::{init_git_repo, write_file, TestDir},
    };

    fn deduper() -> SharedDeduper {
        Arc::new(Mutex::new(Deduper::new(Duration::from_millis(1))))
    }

    #[tokio::test]
    async fn ignored_directory_remove_uses_notify_folder_kind() {
        let tmp = TestDir::new("gongd-event-remove-dir");
        init_git_repo(tmp.path());
        write_file(&tmp.path().join(".gitignore"), "ignored-dir/\n");

        let ignored_dir = tmp.path().join("ignored-dir");
        std::fs::create_dir_all(&ignored_dir).unwrap();
        let repo = RepoState::discover(tmp.path()).unwrap();
        std::fs::remove_dir_all(&ignored_dir).unwrap();

        let event = Event {
            kind: EventKind::Remove(RemoveKind::Folder),
            paths: vec![ignored_dir],
            attrs: EventAttributes::default(),
        };

        let translated = translate_event(&[repo], event, deduper()).await;

        assert!(translated.is_empty());
    }
}
