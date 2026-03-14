use std::{
    fs, io,
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GongdConfig {
    #[serde(default)]
    pub repos: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> io::Result<GongdConfig> {
        if !self.path.exists() {
            return Ok(GongdConfig::default());
        }

        let raw = fs::read_to_string(&self.path)?;
        serde_json::from_str(&raw).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to parse {}: {err}", self.path.display()),
            )
        })
    }

    pub fn save(&self, config: &GongdConfig) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp_path = self.path.with_extension("json.tmp");
        let raw =
            serde_json::to_vec_pretty(config).map_err(|err| io::Error::other(err.to_string()))?;
        fs::write(&tmp_path, raw)?;
        fs::rename(tmp_path, &self.path)?;
        Ok(())
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn watch_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ConfigStore, GongdConfig};
    use crate::test_support::TestDir;

    #[test]
    fn load_missing_config_returns_empty() {
        let tmp = TestDir::new("gongd-config-missing");
        let store = ConfigStore::new(tmp.path().join("missing.json"));

        assert_eq!(store.load().unwrap(), GongdConfig::default());
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = TestDir::new("gongd-config-roundtrip");
        let store = ConfigStore::new(tmp.path().join("gongd.json"));
        let config = GongdConfig {
            repos: vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")],
        };

        store.save(&config).unwrap();

        assert_eq!(store.load().unwrap(), config);
    }
}
