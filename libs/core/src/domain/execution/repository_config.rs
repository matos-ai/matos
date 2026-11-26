use serde::Deserialize;

pub const REPOSITORY_CONFIG_PATH: &str = ".matos.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RepositoryConfig {
    #[serde(default)]
    pub guard: GuardConfig,
    #[serde(default)]
    pub companions: Vec<CompanionConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CompanionConfig {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub path: String,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub ref_file: Option<String>,
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
        let config = RepositoryConfig::parse("[docs]\nname = \"docs\"\n").unwrap();
        assert_eq!(config, RepositoryConfig::default());
    }

    #[test]
    fn a_companion_is_read_with_a_ref_a_ref_file_or_neither() {
        let config = RepositoryConfig::parse(
            "[[companions]]\nrepository = \"acme/sdk\"\npath = \"../sdk\"\nref = \"abc123\"\n\n\
             [[companions]]\nrepository = \"acme/proto\"\npath = \"../proto\"\nref_file = \"proto.ref\"\n\n\
             [[companions]]\nrepository = \"acme/docs\"\npath = \"../docs\"\n",
        )
        .unwrap();
        assert_eq!(config.companions.len(), 3);
        assert_eq!(config.companions[0].reference.as_deref(), Some("abc123"));
        assert_eq!(config.companions[0].ref_file, None);
        assert_eq!(config.companions[1].ref_file.as_deref(), Some("proto.ref"));
        assert_eq!(config.companions[1].reference, None);
        assert_eq!(config.companions[2].reference, None);
        assert_eq!(config.companions[2].ref_file, None);
        assert_eq!(config.companions[2].path, "../docs");
    }

    #[test]
    fn an_entry_with_both_a_ref_and_a_ref_file_still_parses_so_it_can_be_skipped_alone() {
        let config = RepositoryConfig::parse(
            "[[companions]]\nrepository = \"acme/sdk\"\npath = \"../sdk\"\nref = \"a\"\nref_file = \"b\"\n",
        )
        .unwrap();
        assert_eq!(config.companions[0].reference.as_deref(), Some("a"));
        assert_eq!(config.companions[0].ref_file.as_deref(), Some("b"));
    }

    #[test]
    fn a_companion_wrongly_typed_invalidates_the_file() {
        assert!(RepositoryConfig::parse("[[companions]]\npath = 3\n").is_err());
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
