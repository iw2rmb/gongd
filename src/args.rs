use std::{env, io, path::PathBuf};

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "gongd")]
#[command(about = "Watch local Git repositories and broadcast filtered events over a Unix socket")]
pub struct Args {
    /// Unix domain socket path for the event broadcast stream.
    #[arg(long, default_value = "/tmp/gongd.sock")]
    pub socket: PathBuf,

    /// Unix domain socket path for control commands.
    #[arg(long, default_value = "/tmp/gongd.ctl.sock")]
    pub control_socket: PathBuf,

    /// Config file path. Defaults to ~/.gongd.json.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Debounce window in milliseconds for duplicate path+type events.
    #[arg(long, default_value_t = 150)]
    pub debounce_ms: u64,

    /// Optional Git repository roots to watch immediately on startup.
    pub repos: Vec<PathBuf>,
}

impl Args {
    pub fn config_path(&self) -> io::Result<PathBuf> {
        self.config.clone().map_or_else(default_config_path, Ok)
    }
}

fn default_config_path() -> io::Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".gongd.json"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}
