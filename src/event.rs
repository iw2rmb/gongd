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
    folder::MonitoredFolder,
    protocol::{EventType, WireEvent},
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
            Self::Dir => EventType::DirModified,
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
    folder: PathBuf,
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
    folders: &[MonitoredFolder],
    event: Event,
    deduper: SharedDeduper,
) -> Vec<WireEvent> {
    let mut out = Vec::new();

    for path in &event.paths {
        if let Some(event) = translate_path_event(folders, path, event.kind, &deduper).await {
            out.push(event);
        }
    }

    if is_rename_event(event.kind) && event.paths.len() >= 2 {
        let from = &event.paths[0];
        let to = &event.paths[1];
        if let Some(folder) = folder_for(folders, &[from, to]) {
            if !folder.is_inside_git_dir(from) && !folder.is_inside_git_dir(to) {
                if let Some(event) = emit_rename_event(folder, from, to, &deduper).await {
                    out.push(event);
                }
            }
        }
    }

    out
}

async fn translate_path_event(
    folders: &[MonitoredFolder],
    path: &Path,
    kind: EventKind,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let folder = folder_for(folders, &[path])?;
    if folder.is_inside_git_dir(path) {
        translate_git_event(folder, path, kind, deduper).await
    } else {
        translate_folder_event(folder, path, kind, deduper).await
    }
}

async fn translate_folder_event(
    folder: &MonitoredFolder,
    path: &Path,
    kind: EventKind,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let rel = path.strip_prefix(&folder.root).ok()?;
    let entry_kind = worktree_entry_kind(kind, path)?;
    if folder.is_worktree_ignored(rel, entry_kind.is_dir()) {
        return None;
    }

    emit_deduped_event(
        folder,
        PendingEvent::worktree(worktree_event_type(kind, entry_kind)?, rel),
        deduper,
    )
    .await
}

async fn emit_rename_event(
    folder: &MonitoredFolder,
    from: &Path,
    to: &Path,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let rel_from = from.strip_prefix(&folder.root).ok()?;
    let rel_to = to.strip_prefix(&folder.root).ok()?;
    let entry_kind = rename_entry_kind(from, to);

    if folder.is_worktree_ignored(rel_from, entry_kind.is_dir())
        && folder.is_worktree_ignored(rel_to, entry_kind.is_dir())
    {
        return None;
    }

    let event_type = match entry_kind {
        EntryKind::Dir => EventType::DirRenamed,
        EntryKind::File => EventType::FileRenamed,
    };

    emit_deduped_event(
        folder,
        PendingEvent::rename(event_type, rel_from, rel_to),
        deduper,
    )
    .await
}

async fn translate_git_event(
    folder: &MonitoredFolder,
    path: &Path,
    kind: EventKind,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let rel = path.strip_prefix(folder.git_dir()?).ok()?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let event_type = git_event_type(&rel_str);

    match kind {
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {}
        _ => return None,
    }

    emit_deduped_event(folder, PendingEvent::git(event_type, rel_str), deduper).await
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn folder_for<'a>(folders: &'a [MonitoredFolder], paths: &[&Path]) -> Option<&'a MonitoredFolder> {
    folders
        .iter()
        .find(|folder| paths.iter().all(|path| path.starts_with(&folder.root)))
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
        "HEAD" => EventType::GitHeadChanged,
        "index" => EventType::GitIndexChanged,
        "packed-refs" => EventType::GitPackedRefsChanged,
        value if value.starts_with("refs/") => EventType::GitRefsChanged,
        _ => EventType::GitChanged,
    }
}

async fn emit_deduped_event(
    folder: &MonitoredFolder,
    event: PendingEvent,
    deduper: &SharedDeduper,
) -> Option<WireEvent> {
    let key = DedupKey {
        folder: folder.root.clone(),
        event_type: event.event_type,
        path: event.dedup_path,
        git_path: event.dedup_git_path,
    };

    let mut deduper = deduper.lock().await;
    if !deduper.should_emit(key) {
        return None;
    }

    Some(WireEvent {
        folder: folder.root.display().to_string(),
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
        event::{CreateKind, EventAttributes, ModifyKind, RemoveKind},
        Event, EventKind,
    };
    use tokio::sync::Mutex;

    use super::{translate_event, Deduper, SharedDeduper};
    use crate::{
        folder::MonitoredFolder,
        protocol::EventType,
        test_support::{init_git_folder, write_file, TestDir},
    };

    fn deduper() -> SharedDeduper {
        Arc::new(Mutex::new(Deduper::new(Duration::from_millis(1))))
    }

    #[tokio::test]
    async fn ignored_directory_remove_uses_notify_folder_kind() {
        let tmp = TestDir::new("gongd-event-remove-dir");
        init_git_folder(tmp.path());
        write_file(&tmp.path().join(".gitignore"), "ignored-dir/\n");

        let ignored_dir = tmp.path().join("ignored-dir");
        std::fs::create_dir_all(&ignored_dir).unwrap();
        let folder = MonitoredFolder::discover(tmp.path()).unwrap();
        std::fs::remove_dir_all(&ignored_dir).unwrap();

        let event = Event {
            kind: EventKind::Remove(RemoveKind::Folder),
            paths: vec![ignored_dir],
            attrs: EventAttributes::default(),
        };

        let translated = translate_event(&[folder], event, deduper()).await;

        assert!(translated.is_empty());
    }

    #[tokio::test]
    async fn plain_folder_emits_file_event_without_gitignore_filtering() {
        let tmp = TestDir::new("gongd-event-plain-folder");
        write_file(&tmp.path().join(".gitignore"), "ignored.txt\n");
        write_file(&tmp.path().join("ignored.txt"), "");
        let path = std::fs::canonicalize(tmp.path().join("ignored.txt")).unwrap();
        let folder = MonitoredFolder::discover(tmp.path()).unwrap();

        let event = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![path],
            attrs: EventAttributes::default(),
        };

        let translated = translate_event(&[folder], event, deduper()).await;

        assert_eq!(
            translated.iter().map(|e| e.event_type).collect::<Vec<_>>(),
            vec![EventType::FileCreated],
        );
        assert_eq!(translated[0].path.as_deref(), Some("ignored.txt"));
    }

    #[tokio::test]
    async fn directory_modify_emits_dir_modified() {
        let tmp = TestDir::new("gongd-event-modify-dir");
        init_git_folder(tmp.path());

        let dir = tmp.path().join("tracked-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let folder = MonitoredFolder::discover(tmp.path()).unwrap();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Metadata(notify::event::MetadataKind::Any)),
            paths: vec![dir],
            attrs: EventAttributes::default(),
        };

        let translated = translate_event(&[folder], event, deduper()).await;

        assert_eq!(
            translated.iter().map(|e| e.event_type).collect::<Vec<_>>(),
            vec![EventType::DirModified],
        );
    }

    #[tokio::test]
    async fn git_metadata_event_uses_git_event_type() {
        let tmp = TestDir::new("gongd-event-git-head");
        init_git_folder(tmp.path());
        let folder = MonitoredFolder::discover(tmp.path()).unwrap();
        let head = std::fs::canonicalize(tmp.path().join(".git").join("HEAD")).unwrap();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![head],
            attrs: EventAttributes::default(),
        };

        let translated = translate_event(&[folder], event, deduper()).await;

        assert_eq!(
            translated.iter().map(|e| e.event_type).collect::<Vec<_>>(),
            vec![EventType::GitHeadChanged],
        );
        assert_eq!(translated[0].git_path.as_deref(), Some("HEAD"));
    }
}
