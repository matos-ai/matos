use serde::Deserialize;

pub const REPOSITORY_CONFIG_PATH: &str = ".matos.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RepositoryConfig {
    #[serde(default)]
    pub guard: GuardConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardConfig {
    #[serde(default)]
    pub allow_paths: Vec<String>,
    pub max_files: Option<u32>,
    pub max_lines: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid {REPOSITORY_CONFIG_PATH}: {0}")]
pub struct RepositoryConfigError(#[from] toml::de::Error);

impl RepositoryConfig {
    pub fn parse(text: &str) -> Result<Self, RepositoryConfigError> {
        Ok(toml::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_the_default_configuration() {
        assert_eq!(
            RepositoryConfig::parse("").unwrap(),
            RepositoryConfig::default()
        );
    }

    #[test]
    fn the_guard_section_is_read() {
        let config = RepositoryConfig::parse(
            "[guard]\nallow_paths = [\".github/workflows/\"]\nmax_files = 80\nmax_lines = 5000\n",
        )
        .unwrap();
        assert_eq!(config.guard.allow_paths, vec![".github/workflows/"]);
        assert_eq!(config.guard.max_files, Some(80));
        assert_eq!(config.guard.max_lines, Some(5000));
    }

    #[test]
    fn unknown_top_level_sections_are_tolerated() {
        let config = RepositoryConfig::parse("[[companions]]\nname = \"docs\"\n").unwrap();
        assert_eq!(config, RepositoryConfig::default());
    }

    #[test]
    fn an_unknown_guard_key_is_refused() {
        assert!(RepositoryConfig::parse("[guard]\nallow_path = []\n").is_err());
    }

    #[test]
    fn a_wrong_type_or_broken_toml_is_refused() {
        assert!(RepositoryConfig::parse("[guard]\nmax_files = \"many\"\n").is_err());
        assert!(RepositoryConfig::parse("[guard\n").is_err());
        assert!(RepositoryConfig::parse("[guard]\nmax_files = -1\n").is_err());
    }
}
