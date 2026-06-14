use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use ignore::gitignore::{gitconfig_excludes_path, Gitignore, GitignoreBuilder};

use crate::paths::expand_path;

#[derive(Clone, Debug)]
pub struct MonitoredFolder {
    pub root: PathBuf,
    pub git: Option<GitState>,
}

#[derive(Clone, Debug)]
pub struct GitState {
    pub git_dir: PathBuf,
    ignore: Arc<Gitignore>,
}

impl MonitoredFolder {
    pub fn discover(input: &Path) -> io::Result<Self> {
        let input = expand_path(input)?;
        let root = fs::canonicalize(&input)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "folder path must be a directory",
            ));
        }

        let git_dir = root.join(".git");
        let git = if git_dir.is_dir() {
            Some(GitState {
                git_dir,
                ignore: Arc::new(build_ignore_matcher(&root)?),
            })
        } else {
            None
        };

        Ok(Self { root, git })
    }

    pub fn is_inside_git_dir(&self, path: &Path) -> bool {
        self.git
            .as_ref()
            .is_some_and(|git| path.starts_with(&git.git_dir))
    }

    pub fn is_worktree_ignored(&self, rel: &Path, is_dir: bool) -> bool {
        self.git.as_ref().is_some_and(|git| {
            git.ignore
                .matched_path_or_any_parents(rel, is_dir)
                .is_ignore()
        })
    }

    pub fn git_dir(&self) -> Option<&Path> {
        self.git.as_ref().map(|git| git.git_dir.as_path())
    }
}

pub fn normalize_folder_root(path: &Path) -> io::Result<PathBuf> {
    let path = expand_path(path)?;

    if path.exists() {
        fs::canonicalize(path)
    } else if path.is_absolute() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "folder path must exist or be an absolute path that matches a configured watch",
        ))
    }
}

fn build_ignore_matcher(root: &Path) -> io::Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);

    add_folder_gitignores(root, &mut builder);
    add_folder_info_exclude(root, &mut builder);
    add_global_gitignore(&mut builder);

    builder
        .build()
        .map_err(|err| io::Error::other(err.to_string()))
}

fn add_folder_gitignores(root: &Path, builder: &mut GitignoreBuilder) {
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .parents(false)
        .build()
    {
        let Ok(entry) = entry else { continue };

        if entry
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == ".gitignore")
        {
            let _ = builder.add(entry.path());
        }
    }
}

fn add_folder_info_exclude(root: &Path, builder: &mut GitignoreBuilder) {
    let exclude = root.join(".git").join("info").join("exclude");
    if exclude.exists() {
        let _ = builder.add(exclude);
    }
}

fn add_global_gitignore(builder: &mut GitignoreBuilder) {
    if let Some(path) = gitconfig_excludes_path().filter(|path| path.exists()) {
        let _ = builder.add(path);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{normalize_folder_root, MonitoredFolder};
    use crate::test_support::{
        env_lock, init_git_folder, write_file, ScopedCurrentDir, ScopedEnvVar, TestDir,
    };

    fn monitored_folder(root: &Path) -> MonitoredFolder {
        MonitoredFolder::discover(root).unwrap()
    }

    #[test]
    fn discover_accepts_plain_directory_without_git_mode() {
        let root = TestDir::new("gongd-plain-folder");

        let folder = monitored_folder(root.path());

        assert_eq!(folder.root, std::fs::canonicalize(root.path()).unwrap());
        assert!(folder.git.is_none());
    }

    #[test]
    fn discover_enables_git_mode_only_for_git_directory() {
        let root = TestDir::new("gongd-git-folder");
        init_git_folder(root.path());

        let folder = monitored_folder(root.path());

        assert_eq!(
            folder.git_dir().unwrap(),
            std::fs::canonicalize(root.path()).unwrap().join(".git")
        );
    }

    #[test]
    fn discover_treats_git_file_as_plain_folder() {
        let root = TestDir::new("gongd-git-file-folder");
        write_file(&root.path().join(".git"), "gitdir: /tmp/other\n");

        let folder = monitored_folder(root.path());

        assert!(folder.git.is_none());
    }

    #[test]
    fn build_ignore_matcher_filters_paths_under_ignored_directories() {
        let root = TestDir::new("gongd-ignore-parent");
        init_git_folder(root.path());
        write_file(&root.path().join(".gitignore"), "ignored-dir/\n");

        let folder = monitored_folder(root.path());
        let rel = Path::new("ignored-dir/child.txt");

        assert!(folder.is_worktree_ignored(rel, false));
    }

    #[test]
    fn build_ignore_matcher_ignores_git_local_excludesfile_from_launch_directory() {
        let _guard = env_lock().blocking_lock();
        let home = TestDir::new("gongd-home");
        let launcher = TestDir::new("gongd-launcher");
        let watched = TestDir::new("gongd-watched");

        init_git_folder(launcher.path());
        init_git_folder(watched.path());
        write_file(&home.path().join(".gitconfig"), "");
        write_file(
            &launcher.path().join(".git/config"),
            "[core]\n\texcludesFile = /tmp/should-not-apply\n",
        );

        let xdg = home.path().join("xdg");
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _xdg = ScopedEnvVar::set("XDG_CONFIG_HOME", &xdg);
        let _cwd = ScopedCurrentDir::set(launcher.path());

        let folder = monitored_folder(watched.path());

        assert!(!folder.is_worktree_ignored(Path::new("wrongly-ignored"), false));
    }

    #[test]
    fn build_ignore_matcher_respects_global_excludesfile() {
        let _guard = env_lock().blocking_lock();
        let home = TestDir::new("gongd-home");
        let watched = TestDir::new("gongd-watched");
        let global_ignore = home.path().join(".config/git/global-ignore");

        init_git_folder(watched.path());
        write_file(
            &home.path().join(".gitconfig"),
            &format!("[core]\n\texcludesFile = {}\n", global_ignore.display()),
        );
        write_file(&global_ignore, "globally-ignored\n");

        let xdg = home.path().join(".config");
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _xdg = ScopedEnvVar::set("XDG_CONFIG_HOME", &xdg);

        let folder = monitored_folder(watched.path());

        assert!(folder.is_worktree_ignored(Path::new("globally-ignored"), false));
    }

    #[test]
    fn normalize_folder_root_expands_home_prefix() {
        let _guard = env_lock().blocking_lock();
        let home = TestDir::new("gongd-home");
        let folder = home.path().join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());

        let normalized = normalize_folder_root(Path::new("~/folder")).unwrap();

        assert_eq!(normalized, std::fs::canonicalize(folder).unwrap());
    }
}
