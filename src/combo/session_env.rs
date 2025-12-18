use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

use snafu::prelude::*;
use tempfile::{Builder, TempDir};

#[derive(Debug, Snafu)]
pub enum SessionEnvError {
    #[snafu(display("failed to locate coco executable"))]
    CurrentExe { source: std::io::Error },

    #[snafu(display("failed to canonicalize coco executable at {path:?}"))]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to create session tempdir"))]
    CreateTempDir { source: std::io::Error },

    #[snafu(display("failed to prepare session PATH"))]
    JoinPath { source: std::env::JoinPathsError },

    #[snafu(display("failed to create session link {link:?} -> {target:?}"))]
    CreateLink {
        target: PathBuf,
        link: PathBuf,
        source: std::io::Error,
    },
}

pub type Result<T, E = SessionEnvError> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct SessionEnv {
    temp_dir: TempDir,
    coco_path: PathBuf,
    socket_path: PathBuf,
    envs: Vec<(OsString, OsString)>,
}

#[derive(Debug, Clone)]
pub struct SessionEnvBuilder {
    binary_path: Option<PathBuf>,
    command_name: String,
    socket_name: String,
}

impl Default for SessionEnvBuilder {
    fn default() -> Self {
        Self {
            binary_path: None,
            command_name: "coco".to_string(),
            socket_name: "coco.sock".to_string(),
        }
    }
}

impl SessionEnvBuilder {
    pub fn binary_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary_path = Some(path.into());
        self
    }

    pub fn command_name(mut self, name: impl Into<String>) -> Self {
        self.command_name = name.into();
        self
    }

    pub fn socket_name(mut self, name: impl Into<String>) -> Self {
        self.socket_name = name.into();
        self
    }

    pub fn build(self) -> Result<SessionEnv> {
        let binary_path = resolve_binary_path(self.binary_path)?;
        let temp_dir = Builder::new()
            .prefix("coco-session-")
            .tempdir()
            .context(CreateTempDirSnafu)?;

        let coco_path = temp_dir.path().join(&self.command_name);
        create_link(&binary_path, &coco_path).context(CreateLinkSnafu {
            target: binary_path.clone(),
            link: coco_path.clone(),
        })?;

        let socket_path = temp_dir.path().join(self.socket_name);

        let mut paths: Vec<PathBuf> =
            env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect();
        paths.insert(0, temp_dir.path().to_path_buf());
        let joined_path = env::join_paths(paths).context(JoinPathSnafu)?;

        let envs = vec![
            (OsString::from("PATH"), joined_path),
            (
                OsString::from("COCO_SESSION_SOCK"),
                socket_path.clone().into_os_string(),
            ),
        ];

        Ok(SessionEnv {
            temp_dir,
            coco_path,
            socket_path,
            envs,
        })
    }
}

impl SessionEnv {
    pub fn builder() -> SessionEnvBuilder {
        SessionEnvBuilder::default()
    }

    pub fn coco_path(&self) -> &Path {
        &self.coco_path
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn temp_dir(&self) -> &Path {
        self.temp_dir.path()
    }

    pub fn envs(&self) -> Vec<(OsString, OsString)> {
        self.envs.clone()
    }

    pub fn apply_to_command(&self, command: &mut tokio::process::Command) {
        command.envs(self.envs());
    }
}

fn resolve_binary_path(binary_path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match binary_path {
        Some(path) => path,
        None => std::env::current_exe().context(CurrentExeSnafu)?,
    };
    path.canonicalize().context(CanonicalizeSnafu { path })
}

fn create_link(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(not(unix))]
    {
        std::fs::hard_link(target, link).or_else(|_| std::fs::copy(target, link).map(|_| ()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_env() -> Result<(), Box<dyn std::error::Error>> {
        let env = SessionEnv::builder().build()?;
        assert!(env.coco_path().exists());
        assert_eq!(env.socket_path().parent(), Some(env.temp_dir()));

        let envs = env.envs();
        let (_, path) = envs
            .iter()
            .find(|(key, _)| key == "PATH")
            .expect("PATH env is set")
            .clone();
        let mut paths = env::split_paths(&path);
        let first = paths.next().expect("there is at least one path");
        assert_eq!(first, env.temp_dir());

        Ok(())
    }
}
