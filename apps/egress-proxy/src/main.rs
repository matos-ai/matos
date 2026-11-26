use anyhow::Context;
use clap::Parser;
use matos_core::{
    MatosUseCase,
    infrastructure::{crypto::SecretCipher, postgres},
};
use matos_egress_proxy::{Live, Proxy, ProxyConfig, RegistryConfig, config::Args, router};
use matos_github::{GithubApps, GithubBases};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let _telemetry = matos_telemetry::init("matos-egress-proxy", &args.log, &args.observability)?;

    if args.registries.public_base.is_none() {
        tracing::warn!(
            "EGRESS_PUBLIC_BASE is unset: the cargo index configuration cannot be served, crates cannot be downloaded"
        );
    }

    let pool = postgres::connect(&args.database.url, args.database.max_connections)
        .await
        .context("connecting to postgres")?;
    let mut usecase = MatosUseCase::without_policies(pool);
    match &args.secrets.key {
        Some(key) => {
            usecase = usecase.with_secrets_cipher(
                SecretCipher::from_base64(key).context("SECRETS_KEY is unusable")?,
            );
        }
        None => tracing::warn!(
            "SECRETS_KEY is unset: the organizations' Anthropic keys cannot be read, every Anthropic call is refused"
        ),
    }
    let github = GithubApps::new(
        usecase.clone(),
        GithubBases {
            api_base: args.github.api_base.clone(),
            web_base: args.github.web_base.clone(),
        },
    )
    .context("building the GitHub client")?;

    let proxy = Proxy::new(
        Live::new(usecase, github),
        ProxyConfig {
            anthropic_base: args.proxy.anthropic_base.clone(),
            openai_base: args.proxy.openai_base.clone(),
            google_base: args.proxy.google_base.clone(),
            mistral_base: args.proxy.mistral_base.clone(),
            github_base: args.proxy.github_base.clone(),
            registries: RegistryConfig {
                crates_index_base: args.registries.crates_index_base.clone(),
                crates_static_base: args.registries.crates_static_base.clone(),
                npm_base: args.registries.npm_base.clone(),
                public_base: args.registries.public_base.clone(),
            },
        },
    )
    .context("building the upstream client")?;

    let addr = format!("{}:{}", args.proxy.host, args.proxy.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "egress-proxy listening");
    axum::serve(listener, router(proxy))
        .with_graceful_shutdown(matos_telemetry::shutdown_signal())
        .await?;
    Ok(())
}
