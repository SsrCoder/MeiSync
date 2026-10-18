use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use meisync_persistence::fs_store::CompactionPolicy;
use meisync_server::{bootstrap, config_from_parts, router};

/// MeiSync – one file, one YDoc, exposed through WebDAV, S3 and Yjs/Yrs sync.
#[derive(Debug, Parser)]
#[command(name = "meisync", version, about, long_about = None)]
struct Cli {
    /// Data directory (SQLite database + YDoc snapshots / update logs).
    #[arg(long, default_value = "./data", env = "MEISYNC_DATA_DIR")]
    data: PathBuf,

    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:8080", env = "MEISYNC_BIND")]
    bind: String,

    /// Grant every authenticated user write access to public files.
    #[arg(long, default_value_t = false)]
    public_default_write: bool,

    /// Maximum number of YDocs kept in memory.
    #[arg(long, default_value_t = 512)]
    max_cache_documents: usize,

    /// Unload a YDoc after this many seconds without access and without subscribers.
    #[arg(long, default_value_t = 300)]
    idle_unload_secs: u64,

    /// Lifetime of a session token.
    #[arg(long, default_value_t = 30)]
    token_ttl_days: u64,

    /// Maximum accepted request body (WebDAV PUT / S3 PutObject).
    #[arg(long, default_value_t = 64)]
    max_body_mb: usize,

    /// Compact the update log after this many updates.
    #[arg(long, default_value_t = 1000)]
    compact_after_updates: u64,

    /// Compact the update log after this many update bytes.
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    compact_after_bytes: u64,

    /// Compact when the snapshot itself grows beyond this size.
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    compact_snapshot_bytes: u64,

    /// Also serve WebDAV from `/` (so a whole server root can be mounted directly).
    #[arg(long, default_value_t = false)]
    dav_at_root: bool,

    /// Account that is always promoted to administrator at startup.
    #[arg(long, env = "MEISYNC_ADMIN_USER")]
    admin_user: Option<String>,

    /// Password used only when the configured admin account does not exist yet.
    #[arg(long, env = "MEISYNC_ADMIN_PASSWORD")]
    admin_password: Option<String>,

    /// Promote the first registered account when the node has no administrator yet.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    first_user_admin: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "meisync=info,tower_http=info,warn".into()),
        )
        .init();

    let config = config_from_parts(
        cli.data,
        cli.bind.clone(),
        cli.public_default_write,
        cli.max_cache_documents,
        cli.idle_unload_secs,
        cli.token_ttl_days,
        cli.max_body_mb,
        CompactionPolicy {
            max_updates: cli.compact_after_updates,
            max_update_bytes: cli.compact_after_bytes,
            snapshot_bytes: cli.compact_snapshot_bytes,
        },
        cli.admin_user.clone(),
        cli.admin_password.clone(),
        cli.first_user_admin,
    );
    let mut config = config;
    config.dav_at_root = cli.dav_at_root;

    let state = bootstrap(config.clone())
        .await
        .context("failed to boot MeiSync")?;
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("cannot bind {}", config.bind))?;
    tracing::info!(
        data_dir = %config.data_dir.display(),
        bind = %config.bind,
        "MeiSync listening"
    );
    println!(
        "MeiSync is running\n  data:   {}\n  bind:   http://{}\n  admin:  http://{}/admin\n  WebDAV: http://{}/dav\n  S3:     http://{}/s3\n  YSync:  ws://{}/ysync/<path>\n  sync:   http://{}/api/sync\n  API:    http://{}/api/health",
        config.data_dir.display(),
        config.bind,
        config.bind,
        config.bind,
        config.bind,
        config.bind,
        config.bind,
        config.bind,
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
