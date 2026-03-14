use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use ignore::gitignore::{gitconfig_excludes_path, Gitignore, GitignoreBuilder};

#[derive(Clone, Debug)]
pub struct RepoState {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    ignore: Arc<Gitignore>,
}

impl RepoState {
    pub fn discover(input: &Path) -> io::Result<Self> {
        let root = fs::canonicalize(input)?;
        let git_dir = root.join(".git");
        if !git_dir.exists() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no .git directory found",
            ));
        }
        if !git_dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                ".git is not a directory (git worktrees and gitdir files are not yet supported)",
            ));
        }

        let ignore = build_ignore_matcher(&root)?;
        Ok(Self {
            root,
            git_dir,
            ignore: Arc::new(ignore),
        })
    }

    pub fn is_inside_git_dir(&self, path: &Path) -> bool {
        path.starts_with(&self.git_dir)
    }

    pub fn is_worktree_ignored(&self, rel: &Path, is_dir: bool) -> bool {
        self.ignore
            .matched_path_or_any_parents(rel, is_dir)
            .is_ignore()
    }
}

pub fn build_startup_repos(paths: Vec<PathBuf>) -> Vec<RepoState> {
    let mut repos = Vec::new();
    let mut seen = HashSet::new();

    for input in paths {
        match RepoState::discover(&input) {
            Ok(repo) => {
                if seen.insert(repo.root.clone()) {
                    repos.push(repo);
                }
            }
            Err(err) => eprintln!("skipping {}: {err}", input.display()),
        }
    }

    repos
}

pub fn normalize_repo_root(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        fs::canonicalize(path)
    } else if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "repo path must exist or be an absolute path that matches a configured watch",
        ))
    }
}

fn build_ignore_matcher(root: &Path) -> io::Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);

    add_repo_gitignores(root, &mut builder)?;
    add_repo_info_exclude(root, &mut builder)?;
    add_global_gitignore(&mut builder);

    builder
        .build()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))
}

fn add_repo_gitignores(root: &Path, builder: &mut GitignoreBuilder) -> io::Result<()> {
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .parents(false)
        .build()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        if entry
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == ".gitignore")
            .unwrap_or(false)
        {
            let _ = builder.add(entry.path());
        }
    }
    Ok(())
}

fn add_repo_info_exclude(root: &Path, builder: &mut GitignoreBuilder) -> io::Result<()> {
    let exclude = root.join(".git").join("info").join("exclude");
    if exclude.exists() {
        let _ = builder.add(exclude);
    }
    Ok(())
}

fn add_global_gitignore(builder: &mut GitignoreBuilder) {
    if let Some(path) = gitconfig_excludes_path().filter(|path| path.exists()) {
        let _ = builder.add(path);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::RepoState;
    use crate::test_support::{
        env_lock, init_git_repo, write_file, ScopedCurrentDir, ScopedEnvVar, TestDir,
    };

    fn repo_state(root: &Path) -> RepoState {
        RepoState::discover(root).unwrap()
    }

    #[test]
    fn build_ignore_matcher_filters_paths_under_ignored_directories() {
        let root = TestDir::new("gongd-ignore-parent");
        init_git_repo(root.path());
        write_file(&root.path().join(".gitignore"), "ignored-dir/\n");

        let repo = repo_state(root.path());
        let rel = Path::new("ignored-dir/child.txt");

        assert!(repo.is_worktree_ignored(rel, false));
    }

    #[test]
    fn build_ignore_matcher_ignores_repo_local_excludesfile_from_launch_directory() {
        let _guard = env_lock().lock().unwrap();
        let home = TestDir::new("gongd-home");
        let launcher = TestDir::new("gongd-launcher");
        let watched = TestDir::new("gongd-watched");

        init_git_repo(launcher.path());
        init_git_repo(watched.path());
        write_file(&home.path().join(".gitconfig"), "");
        write_file(
            &launcher.path().join(".git/config"),
            "[core]\n\texcludesFile = /tmp/should-not-apply\n",
        );

        let xdg = home.path().join("xdg");
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _xdg = ScopedEnvVar::set("XDG_CONFIG_HOME", &xdg);
        let _cwd = ScopedCurrentDir::set(launcher.path());

        let repo = repo_state(watched.path());

        assert!(!repo.is_worktree_ignored(Path::new("wrongly-ignored"), false));
    }

    #[test]
    fn build_ignore_matcher_respects_global_excludesfile() {
        let _guard = env_lock().lock().unwrap();
        let home = TestDir::new("gongd-home");
        let watched = TestDir::new("gongd-watched");
        let global_ignore = home.path().join(".config/git/global-ignore");

        init_git_repo(watched.path());
        write_file(
            &home.path().join(".gitconfig"),
            &format!("[core]\n\texcludesFile = {}\n", global_ignore.display()),
        );
        write_file(&global_ignore, "globally-ignored\n");

        let xdg = home.path().join(".config");
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _xdg = ScopedEnvVar::set("XDG_CONFIG_HOME", &xdg);

        let repo = repo_state(watched.path());

        assert!(repo.is_worktree_ignored(Path::new("globally-ignored"), false));
    }
}
