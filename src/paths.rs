use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error("the operating system did not provide a {0} directory")]
    MissingDirectory(&'static str),
    #[error("the operating system provided a relative {0} directory")]
    RelativeDirectory(&'static str),
}

/// Standard per-user locations used by SnenkBot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
}

impl AppPaths {
    /// Resolves platform-standard per-user directories without creating them.
    pub fn resolve() -> Result<Self, PathError> {
        #[cfg(target_os = "macos")]
        {
            let root =
                dirs::data_dir().ok_or(PathError::MissingDirectory("application support"))?;
            Self::from_roots(Some(&root), Some(&root), Some(&root), "SnenkBot")
        }

        #[cfg(target_os = "windows")]
        {
            let root = dirs::data_local_dir()
                .ok_or(PathError::MissingDirectory("local application data"))?;
            Self::from_roots(Some(&root), Some(&root), Some(&root), "SnenkBot")
        }

        #[cfg(target_os = "linux")]
        {
            Self::from_roots(
                dirs::config_dir().as_deref(),
                dirs::data_dir().as_deref(),
                dirs::state_dir().as_deref(),
                "snenkbot",
            )
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        Err(PathError::MissingDirectory("per-user application"))
    }

    fn from_roots(
        config: Option<&Path>,
        data: Option<&Path>,
        state: Option<&Path>,
        app_name: &str,
    ) -> Result<Self, PathError> {
        let config = config.ok_or(PathError::MissingDirectory("config"))?;
        let data = data.ok_or(PathError::MissingDirectory("data"))?;
        let state = state.ok_or(PathError::MissingDirectory("state"))?;
        for (name, root) in [("config", config), ("data", data), ("state", state)] {
            if !root.is_absolute() {
                return Err(PathError::RelativeDirectory(name));
            }
        }

        let root = |path: &Path| path.join(app_name);
        Ok(Self {
            config: root(config),
            data: root(data),
            state: root(state),
        })
    }

    pub fn workflows_dir(&self) -> PathBuf {
        self.data.join("workflows")
    }

    pub fn integrations_dir(&self) -> PathBuf {
        self.config.join("integrations")
    }

    pub fn history_dir(&self) -> PathBuf {
        self.state.join("history")
    }
}

#[cfg(test)]
mod tests {
    use super::AppPaths;
    use super::PathError;
    use std::path::{Path, PathBuf};

    fn test_roots() -> (PathBuf, PathBuf, PathBuf) {
        #[cfg(windows)]
        let prefix = Path::new("C:/user");
        #[cfg(not(windows))]
        let prefix = Path::new("/user");

        (
            prefix.join("config"),
            prefix.join("data"),
            prefix.join("state"),
        )
    }

    #[test]
    fn maps_platform_roots_to_application_directories() {
        let (config, data, state) = test_roots();
        let paths =
            AppPaths::from_roots(Some(&config), Some(&data), Some(&state), "snenkbot").unwrap();

        assert_eq!(paths.config, config.join("snenkbot"));
        assert_eq!(paths.data, data.join("snenkbot"));
        assert_eq!(paths.state, state.join("snenkbot"));
        assert_eq!(paths.workflows_dir(), data.join("snenkbot/workflows"));
        assert_eq!(
            paths.integrations_dir(),
            config.join("snenkbot/integrations")
        );
        assert_eq!(paths.history_dir(), state.join("snenkbot/history"));
    }

    #[test]
    fn shared_platform_root_maps_all_locations_to_the_same_application_directory() {
        #[cfg(windows)]
        let root = Path::new("C:/user/Application Support");
        #[cfg(not(windows))]
        let root = Path::new("/user/Application Support");
        let paths = AppPaths::from_roots(Some(root), Some(root), Some(root), "SnenkBot").unwrap();
        let app_root = root.join("SnenkBot");

        assert_eq!(paths.config, app_root);
        assert_eq!(paths.data, app_root);
        assert_eq!(paths.state, app_root);
    }

    #[test]
    fn rejects_relative_roots() {
        let error = AppPaths::from_roots(
            Some(Path::new("relative/config")),
            Some(Path::new("/user/data")),
            Some(Path::new("/user/state")),
            "snenkbot",
        )
        .unwrap_err();

        assert_eq!(error, PathError::RelativeDirectory("config"));
    }

    #[test]
    fn rejects_missing_roots() {
        let error = AppPaths::from_roots(
            None,
            Some(Path::new("/user/data")),
            Some(Path::new("/user/state")),
            "snenkbot",
        )
        .unwrap_err();

        assert_eq!(error, PathError::MissingDirectory("config"));
    }
}
