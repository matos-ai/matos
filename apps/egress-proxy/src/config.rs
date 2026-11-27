use clap::Parser;
use matos_args::{
    DatabaseArgs, EgressProxyArgs, GithubArgs, LogArgs, ObservabilityArgs, SecretsArgs,
};

#[derive(Debug, Clone, clap::Args)]
pub struct RegistryArgs {
    #[arg(
        id = "egress_crates_index_base",
        long = "egress-crates-index-base",
        env = "EGRESS_CRATES_INDEX_BASE",
        default_value = "https://index.crates.io"
    )]
    pub crates_index_base: String,

    #[arg(
        id = "egress_crates_static_base",
        long = "egress-crates-static-base",
        env = "EGRESS_CRATES_STATIC_BASE",
        default_value = "https://static.crates.io/crates"
    )]
    pub crates_static_base: String,

    #[arg(
        id = "egress_npm_base",
        long = "egress-npm-base",
        env = "EGRESS_NPM_BASE",
        default_value = "https://registry.npmjs.org"
    )]
    pub npm_base: String,

    #[arg(
        id = "egress_public_base",
        long = "egress-public-base",
        env = "EGRESS_PUBLIC_BASE"
    )]
    pub public_base: Option<String>,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "matos-egress-proxy", version)]
pub struct Args {
    #[command(flatten)]
    pub proxy: EgressProxyArgs,
    #[command(flatten)]
    pub registries: RegistryArgs,
    #[command(flatten)]
    pub github: GithubArgs,
    #[command(flatten)]
    pub secrets: SecretsArgs,
    #[command(flatten)]
    pub database: DatabaseArgs,
    #[command(flatten)]
    pub log: LogArgs,
    #[command(flatten)]
    pub observability: ObservabilityArgs,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_definition_is_valid() {
        <Args as clap::CommandFactory>::command().debug_assert();
    }

    #[test]
    fn defaults_apply() {
        let args = Args::parse_from(["bin", "--database-url", "postgres://x"]);
        assert_eq!(args.proxy.port, 4130);
        assert_eq!(args.proxy.anthropic_base, "https://api.anthropic.com");
        assert_eq!(args.proxy.github_base, "https://github.com");
        assert_eq!(args.proxy.mistral_base, "https://api.mistral.ai");
    }

    #[test]
    fn the_registries_default_to_the_public_mirrors_and_can_be_overridden() {
        let args = Args::parse_from(["bin", "--database-url", "postgres://x"]);
        assert_eq!(args.registries.crates_index_base, "https://index.crates.io");
        assert_eq!(
            args.registries.crates_static_base,
            "https://static.crates.io/crates"
        );
        assert_eq!(args.registries.npm_base, "https://registry.npmjs.org");
        assert_eq!(args.registries.public_base, None);
        let args = Args::parse_from([
            "bin",
            "--database-url",
            "postgres://x",
            "--egress-npm-base",
            "http://npm:1",
            "--egress-public-base",
            "http://proxy:4130",
        ]);
        assert_eq!(args.registries.npm_base, "http://npm:1");
        assert_eq!(
            args.registries.public_base.as_deref(),
            Some("http://proxy:4130")
        );
    }
}
