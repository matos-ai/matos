use super::repository_config::GuardConfig;

pub const DEFAULT_DENY_PATHS: [&str; 13] = [
    ".github/workflows/",
    "deploy/",
    "charts/",
    "helm/",
    "terraform/",
    "k8s/",
    ".env*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "*.p12",
    "credentials*",
    ".matos.toml",
];

pub const DEFAULT_MAX_FILES: u32 = 50;
pub const DEFAULT_MAX_LINES: u32 = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushGuard {
    pub deny_paths: Vec<String>,
    pub allow_paths: Vec<String>,
    pub max_files: u32,
    pub max_lines: u32,
}

impl PushGuard {
    pub fn from_config(config: &GuardConfig) -> Self {
        Self {
            deny_paths: DEFAULT_DENY_PATHS
                .iter()
                .map(|path| (*path).to_owned())
                .collect(),
            allow_paths: config
                .allow_paths
                .iter()
                .map(|path| path.trim().to_owned())
                .filter(|path| !path.is_empty() && !path.contains('\n'))
                .collect(),
            max_files: config.max_files.unwrap_or(DEFAULT_MAX_FILES),
            max_lines: config.max_lines.unwrap_or(DEFAULT_MAX_LINES),
        }
    }

    pub fn deny_lines(&self) -> String {
        self.deny_paths.join("\n")
    }

    pub fn allow_lines(&self) -> String {
        self.allow_paths.join("\n")
    }
}

impl Default for PushGuard {
    fn default() -> Self {
        Self::from_config(&GuardConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_deny_the_sensitive_paths_and_cap_the_size() {
        let guard = PushGuard::default();
        assert_eq!(guard.deny_paths.len(), 13);
        assert!(guard.deny_paths.contains(&".github/workflows/".to_owned()));
        assert!(guard.deny_paths.contains(&".matos.toml".to_owned()));
        assert!(guard.allow_paths.is_empty());
        assert_eq!((guard.max_files, guard.max_lines), (50, 2000));
    }

    #[test]
    fn the_configuration_raises_the_limits_and_allows_paths_but_never_lifts_the_denials() {
        let guard = PushGuard::from_config(&GuardConfig {
            allow_paths: vec![" .github/workflows/ ".into(), "".into()],
            max_files: Some(80),
            max_lines: Some(9000),
        });
        assert_eq!(guard.allow_paths, vec![".github/workflows/"]);
        assert_eq!((guard.max_files, guard.max_lines), (80, 9000));
        assert_eq!(guard.deny_paths, PushGuard::default().deny_paths);
    }

    #[test]
    fn the_entries_reach_the_container_one_per_line() {
        let guard = PushGuard::from_config(&GuardConfig {
            allow_paths: vec!["a/".into(), "b/".into()],
            ..GuardConfig::default()
        });
        assert_eq!(guard.allow_lines(), "a/\nb/");
        assert_eq!(guard.deny_lines().lines().count(), 13);
    }
}
