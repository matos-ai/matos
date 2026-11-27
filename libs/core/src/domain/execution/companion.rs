use super::{RepositoryRef, repository_config::CompanionConfig, valid_branch_name};
use crate::domain::crew::RepositoryGrants;

pub const WORKSPACE: &str = "/workspace";
pub const CHECKOUT: &str = "/workspace/repo";
pub const MAX_COMPANIONS: usize = 8;

const LABEL_MAX: usize = 100;
const REF_FILE_MAX: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Companion {
    pub repository: RepositoryRef,
    pub path: String,
    pub reference: Option<String>,
}

impl Companion {
    pub fn line(&self) -> String {
        format!(
            "{}\t{}\t{}",
            self.repository,
            self.path,
            self.reference.as_deref().unwrap_or("")
        )
    }
}

pub fn companion_lines(companions: &[Companion]) -> String {
    companions
        .iter()
        .map(Companion::line)
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefSource {
    Default,
    Fixed(String),
    File(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCompanion {
    pub repository: RepositoryRef,
    pub path: String,
    pub source: RefSource,
}

impl PlannedCompanion {
    pub fn resolved(self, file_content: Option<&str>) -> Result<Companion, CompanionSkip> {
        let reference = match (&self.source, file_content) {
            (RefSource::Default, _) => None,
            (RefSource::Fixed(reference), _) => Some(reference.clone()),
            (RefSource::File(_), None) => return Err(CompanionSkip::RefFileMissing),
            (RefSource::File(_), Some(content)) => {
                let reference = content.trim();
                if reference.is_empty() {
                    return Err(CompanionSkip::RefFileMissing);
                }
                if !valid_branch_name(reference) {
                    return Err(CompanionSkip::InvalidEntry);
                }
                Some(reference.to_owned())
            }
        };
        Ok(Companion {
            repository: self.repository,
            path: self.path,
            reference,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanionSkip {
    NotGranted,
    InvalidPath,
    InvalidEntry,
    RefFileMissing,
    TooMany,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCompanion {
    pub label: String,
    pub reason: CompanionSkip,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompanionPlan {
    pub planned: Vec<PlannedCompanion>,
    pub skipped: Vec<SkippedCompanion>,
}

impl CompanionPlan {
    pub fn from_config(entries: &[CompanionConfig], grants: Option<&RepositoryGrants>) -> Self {
        let mut plan = Self::default();
        for entry in entries {
            let label = label_of(&entry.repository);
            match plan.accept(entry, grants) {
                Ok(planned) => plan.planned.push(planned),
                Err(reason) => plan.skipped.push(SkippedCompanion { label, reason }),
            }
        }
        plan
    }

    fn accept(
        &self,
        entry: &CompanionConfig,
        grants: Option<&RepositoryGrants>,
    ) -> Result<PlannedCompanion, CompanionSkip> {
        let repository: RepositoryRef = entry
            .repository
            .trim()
            .parse()
            .map_err(|_| CompanionSkip::InvalidEntry)?;
        let source = ref_source(entry)?;
        if !grants.is_some_and(|grants| grants.allows(&repository)) {
            return Err(CompanionSkip::NotGranted);
        }
        let path = resolve_path(&entry.path).ok_or(CompanionSkip::InvalidPath)?;
        if self.planned.iter().any(|planned| planned.path == path) {
            return Err(CompanionSkip::InvalidPath);
        }
        if self.planned.len() >= MAX_COMPANIONS {
            return Err(CompanionSkip::TooMany);
        }
        Ok(PlannedCompanion {
            repository,
            path,
            source,
        })
    }
}

fn ref_source(entry: &CompanionConfig) -> Result<RefSource, CompanionSkip> {
    match (entry.reference.as_deref(), entry.ref_file.as_deref()) {
        (None, None) => Ok(RefSource::Default),
        (Some(reference), None) => {
            let reference = reference.trim();
            if valid_branch_name(reference) {
                Ok(RefSource::Fixed(reference.to_owned()))
            } else {
                Err(CompanionSkip::InvalidEntry)
            }
        }
        (None, Some(file)) => {
            let file = file.trim();
            if valid_ref_file(file) {
                Ok(RefSource::File(file.to_owned()))
            } else {
                Err(CompanionSkip::InvalidEntry)
            }
        }
        (Some(_), Some(_)) => Err(CompanionSkip::InvalidEntry),
    }
}

fn valid_ref_file(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= REF_FILE_MAX
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn label_of(declared: &str) -> String {
    let label: String = declared
        .trim()
        .chars()
        .take(LABEL_MAX)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') {
                c
            } else {
                '?'
            }
        })
        .collect();
    if label.is_empty() {
        "?".to_owned()
    } else {
        label
    }
}

pub fn resolve_path(declared: &str) -> Option<String> {
    let declared = declared.trim();
    if declared.is_empty()
        || declared.starts_with('/')
        || !declared
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return None;
    }
    let mut parts: Vec<&str> = CHECKOUT
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for segment in declared.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    let inside_workspace = parts.first() == Some(&"workspace") && parts.len() >= 2;
    let inside_checkout = parts.starts_with(&["workspace", "repo"]);
    (inside_workspace && !inside_checkout).then(|| format!("/{}", parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(repository: &str, path: &str) -> CompanionConfig {
        CompanionConfig {
            repository: repository.into(),
            path: path.into(),
            ..CompanionConfig::default()
        }
    }

    fn grants(repositories: &[&str]) -> RepositoryGrants {
        RepositoryGrants::try_from(
            repositories
                .iter()
                .map(|repository| (*repository).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn a_sibling_path_resolves_next_to_the_checkout() {
        assert_eq!(resolve_path("../sdk").as_deref(), Some("/workspace/sdk"));
        assert_eq!(
            resolve_path("../vendor/sdk").as_deref(),
            Some("/workspace/vendor/sdk")
        );
        assert_eq!(resolve_path("../a/../b").as_deref(), Some("/workspace/b"));
        assert_eq!(resolve_path(" ../sdk ").as_deref(), Some("/workspace/sdk"));
    }

    #[test]
    fn a_path_outside_the_workspace_or_in_the_checkout_is_refused() {
        for bad in [
            "",
            "/etc",
            "/workspace/sdk",
            "..",
            "../..",
            "../../etc",
            "../../../..",
            ".",
            "./",
            "sdk",
            "./sdk",
            "../repo",
            "../repo/vendor/sdk",
            "../sdk/../repo",
            "../sd k",
            "../sdk;rm",
            "../sdk\n../x",
            "../$HOME",
            "../s\tdk",
        ] {
            assert_eq!(resolve_path(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_granted_companion_is_planned_with_its_resolved_path() {
        let plan = CompanionPlan::from_config(
            &[entry("acme/sdk", "../sdk")],
            Some(&grants(&["Acme/SDK"])),
        );
        assert_eq!(plan.skipped, vec![]);
        assert_eq!(
            plan.planned,
            vec![PlannedCompanion {
                repository: "acme/sdk".parse().unwrap(),
                path: "/workspace/sdk".into(),
                source: RefSource::Default,
            }]
        );
    }

    #[test]
    fn a_companion_the_agent_is_not_granted_is_skipped_with_its_name() {
        let plan = CompanionPlan::from_config(
            &[entry("acme/sdk", "../sdk"), entry("acme/other", "../other")],
            Some(&grants(&["acme/sdk"])),
        );
        assert_eq!(plan.planned.len(), 1);
        assert_eq!(
            plan.skipped,
            vec![SkippedCompanion {
                label: "acme/other".into(),
                reason: CompanionSkip::NotGranted
            }]
        );
    }

    #[test]
    fn a_run_without_a_registry_agent_gets_no_companion() {
        let plan = CompanionPlan::from_config(&[entry("acme/sdk", "../sdk")], None);
        assert!(plan.planned.is_empty());
        assert_eq!(plan.skipped[0].reason, CompanionSkip::NotGranted);
    }

    #[test]
    fn a_bad_path_or_a_shared_path_is_skipped_as_invalid() {
        let plan = CompanionPlan::from_config(
            &[
                entry("acme/a", "../a"),
                entry("acme/b", "../a"),
                entry("acme/c", "/etc"),
                entry("acme/d", "repo/d"),
            ],
            Some(&grants(&["acme/a", "acme/b", "acme/c", "acme/d"])),
        );
        assert_eq!(plan.planned.len(), 1);
        assert_eq!(
            plan.skipped
                .iter()
                .map(|skipped| (skipped.label.as_str(), skipped.reason))
                .collect::<Vec<_>>(),
            vec![
                ("acme/b", CompanionSkip::InvalidPath),
                ("acme/c", CompanionSkip::InvalidPath),
                ("acme/d", CompanionSkip::InvalidPath),
            ]
        );
    }

    #[test]
    fn a_ref_a_ref_file_both_or_neither_pick_the_commit() {
        let with = |reference: Option<&str>, ref_file: Option<&str>| CompanionConfig {
            reference: reference.map(str::to_owned),
            ref_file: ref_file.map(str::to_owned),
            ..entry("acme/sdk", "../sdk")
        };
        let granted = grants(&["acme/sdk"]);
        let plan = |entry: CompanionConfig| CompanionPlan::from_config(&[entry], Some(&granted));
        assert_eq!(
            plan(with(Some("abc1234"), None)).planned[0].source,
            RefSource::Fixed("abc1234".into())
        );
        assert_eq!(
            plan(with(None, Some("sdk.ref"))).planned[0].source,
            RefSource::File("sdk.ref".into())
        );
        assert_eq!(plan(with(None, None)).planned[0].source, RefSource::Default);
        let both = plan(with(Some("abc"), Some("sdk.ref")));
        assert!(both.planned.is_empty());
        assert_eq!(both.skipped[0].reason, CompanionSkip::InvalidEntry);
        for bad_ref in ["-x", "a b", "a..b", ""] {
            let bad = plan(with(Some(bad_ref), None));
            assert_eq!(
                bad.skipped[0].reason,
                CompanionSkip::InvalidEntry,
                "{bad_ref}"
            );
        }
        for bad_file in ["../sdk.ref", "/etc/passwd", "a//b", "", "a b"] {
            let bad = plan(with(None, Some(bad_file)));
            assert_eq!(
                bad.skipped[0].reason,
                CompanionSkip::InvalidEntry,
                "{bad_file}"
            );
        }
    }

    #[test]
    fn an_entry_without_a_valid_repository_is_invalid() {
        let granted = grants(&["acme/sdk"]);
        for repository in ["", "acme", "acme/sdk/x", "ac me/sdk"] {
            let plan = CompanionPlan::from_config(&[entry(repository, "../sdk")], Some(&granted));
            assert_eq!(plan.skipped[0].reason, CompanionSkip::InvalidEntry);
        }
    }

    #[test]
    fn a_skipped_label_cannot_carry_markup_or_mentions() {
        let plan = CompanionPlan::from_config(&[entry("`@a`\n[x](y)", "../a")], None);
        assert!(
            plan.skipped[0]
                .label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '?' | '-' | '_' | '.' | '/'))
        );
    }

    #[test]
    fn companions_beyond_the_cap_are_skipped() {
        let names: Vec<String> = (0..MAX_COMPANIONS + 2)
            .map(|i| format!("acme/r{i}"))
            .collect();
        let entries: Vec<CompanionConfig> = names
            .iter()
            .enumerate()
            .map(|(i, name)| entry(name, &format!("../r{i}")))
            .collect();
        let granted = RepositoryGrants::try_from(names).unwrap();
        let plan = CompanionPlan::from_config(&entries, Some(&granted));
        assert_eq!(plan.planned.len(), MAX_COMPANIONS);
        assert_eq!(plan.skipped.len(), 2);
        assert_eq!(plan.skipped[0].reason, CompanionSkip::TooMany);
    }

    fn planned(source: RefSource) -> PlannedCompanion {
        PlannedCompanion {
            repository: "acme/sdk".parse().unwrap(),
            path: "/workspace/sdk".into(),
            source,
        }
    }

    #[test]
    fn a_ref_file_gives_its_trimmed_content_as_the_ref() {
        let file = planned(RefSource::File("sdk.ref".into()));
        let companion = file.clone().resolved(Some("  0a1b2c3d4e5f  \n")).unwrap();
        assert_eq!(companion.reference.as_deref(), Some("0a1b2c3d4e5f"));
        assert_eq!(
            file.clone().resolved(None),
            Err(CompanionSkip::RefFileMissing)
        );
        assert_eq!(
            file.clone().resolved(Some(" \n")),
            Err(CompanionSkip::RefFileMissing)
        );
        assert_eq!(
            file.resolved(Some("--upload-pack=x")),
            Err(CompanionSkip::InvalidEntry)
        );
        assert_eq!(
            planned(RefSource::Default)
                .resolved(None)
                .unwrap()
                .reference,
            None
        );
        assert_eq!(
            planned(RefSource::Fixed("main".into()))
                .resolved(None)
                .unwrap()
                .reference
                .as_deref(),
            Some("main")
        );
    }

    #[test]
    fn the_container_gets_one_companion_per_line_with_tabs() {
        let sdk = Companion {
            repository: "acme/sdk".parse().unwrap(),
            path: "/workspace/sdk".into(),
            reference: Some("abc".into()),
        };
        let docs = Companion {
            repository: "acme/docs".parse().unwrap(),
            path: "/workspace/docs".into(),
            reference: None,
        };
        assert_eq!(
            companion_lines(&[sdk, docs]),
            "acme/sdk\t/workspace/sdk\tabc\nacme/docs\t/workspace/docs\t"
        );
        assert_eq!(companion_lines(&[]), "");
    }
}
