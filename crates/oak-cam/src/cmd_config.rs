//! `oak-cam config init [--path P]` and `oak-cam config show [--path P]`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::{Args, Subcommand};
use oak_core::config::{self, Config};

/// `oak-cam config ...`.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: ConfigAction,
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Write the default config (with comments) if the file does not exist.
    Init {
        /// Config file (default: the per-user config path).
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Print the effective config (after validation) as TOML.
    Show {
        /// Config file (default: the per-user config path).
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

/// The given path, or the per-user default.
pub fn resolve(path: Option<&Path>) -> Result<PathBuf> {
    match path {
        Some(p) => Ok(p.to_owned()),
        None => Config::default_path().context("no config path"),
    }
}

/// Loads `path` if it exists, else the defaults. The second value says
/// whether the file existed.
pub fn load_or_default(path: &Path) -> Result<(Config, bool)> {
    Ok(match Config::load_if_exists(path)? {
        Some(c) => (c, true),
        None => (Config::default(), false),
    })
}

/// Runs a `config` subcommand.
pub fn run(out: &mut impl Write, args: &ConfigArgs) -> Result<()> {
    match &args.action {
        ConfigAction::Init { path } => {
            let path = resolve(path.as_deref())?;
            if config::init_file(&path)? {
                writeln!(out, "wrote default config to {}", path.display())?;
            } else {
                writeln!(out, "{} already exists; left unchanged", path.display())?;
            }
        }
        ConfigAction::Show { path } => {
            let path = resolve(path.as_deref())?;
            let (mut cfg, existed) = load_or_default(&path)?;
            writeln!(out, "# {}", path.display())?;
            if !existed {
                writeln!(out, "# (file not found: showing the defaults)")?;
            }
            for w in cfg.validate() {
                writeln!(out, "# adjusted: {w}")?;
            }
            write!(out, "{}", cfg.to_toml()?)?;
        }
    }
    Ok(())
}
