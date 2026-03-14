use std::{
    env, io,
    path::{Path, PathBuf},
};

pub fn expand_path(path: &Path) -> io::Result<PathBuf> {
    let Some(raw) = path.to_str() else {
        return Ok(path.to_path_buf());
    };

    let raw = expand_home(raw)?;
    Ok(PathBuf::from(expand_env_vars(&raw)?))
}

fn expand_home(raw: &str) -> io::Result<String> {
    if raw == "~" {
        return home_dir_string();
    }

    if let Some(rest) = raw.strip_prefix("~/") {
        let mut home = PathBuf::from(home_dir_string()?);
        home.push(rest);
        return Ok(home.to_string_lossy().into_owned());
    }

    Ok(raw.to_owned())
}

fn home_dir_string() -> io::Result<String> {
    env::var_os("HOME")
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

fn expand_env_vars(raw: &str) -> io::Result<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut expanded = String::new();
    let mut index = 0;

    while index < chars.len() {
        if chars[index] != '$' {
            expanded.push(chars[index]);
            index += 1;
            continue;
        }

        if index + 1 >= chars.len() {
            expanded.push('$');
            index += 1;
            continue;
        }

        if chars[index + 1] == '{' {
            let mut end = index + 2;
            while end < chars.len() && chars[end] != '}' {
                end += 1;
            }

            if end >= chars.len() {
                expanded.push('$');
                index += 1;
                continue;
            }

            let name: String = chars[index + 2..end].iter().collect();
            if name.is_empty() {
                expanded.push_str("${}");
                index = end + 1;
                continue;
            }

            expanded.push_str(&env_var_value(&name)?);
            index = end + 1;
            continue;
        }

        if !is_env_var_start(chars[index + 1]) {
            expanded.push('$');
            index += 1;
            continue;
        }

        let mut end = index + 2;
        while end < chars.len() && is_env_var_continue(chars[end]) {
            end += 1;
        }

        let name: String = chars[index + 1..end].iter().collect();
        expanded.push_str(&env_var_value(&name)?);
        index = end;
    }

    Ok(expanded)
}

fn env_var_value(name: &str) -> io::Result<String> {
    env::var_os(name)
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("environment variable {name} is not set"),
            )
        })
}

fn is_env_var_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_env_var_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use std::{io::ErrorKind, path::Path};

    use super::expand_path;
    use crate::test_support::{env_lock, ScopedEnvVar, TestDir};

    #[test]
    fn expand_path_expands_home_and_env_vars() {
        let _guard = env_lock().lock().unwrap();
        let home = TestDir::new("gongd-paths-home");
        let workspace = TestDir::new("gongd-paths-workspace");
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _workspace = ScopedEnvVar::set("WORKSPACE", workspace.path());

        assert_eq!(
            expand_path(Path::new("~/repo")).unwrap(),
            home.path().join("repo")
        );
        assert_eq!(
            expand_path(Path::new("$WORKSPACE/repo")).unwrap(),
            workspace.path().join("repo")
        );
        assert_eq!(
            expand_path(Path::new("${WORKSPACE}/repo")).unwrap(),
            workspace.path().join("repo")
        );
    }

    #[test]
    fn expand_path_errors_for_unset_env_vars() {
        let _guard = env_lock().lock().unwrap();
        let err = expand_path(Path::new("$GONGD_TEST_UNSET/repo")).unwrap_err();

        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
}
