//! Live probes, not production request handlers.
use anyhow::{Result, ensure};
use sqlx::{
    Connection, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use std::{sync::OnceLock, time::Duration};

static POOL: OnceLock<PgPool> = OnceLock::new();
static REUSE_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn options(ca: &str) -> Result<PgConnectOptions> {
    Ok(std::env::var("DATABASE_URL")?
        .parse::<PgConnectOptions>()?
        .ssl_mode(PgSslMode::VerifyFull)
        .ssl_root_cert_from_pem(std::fs::read(ca)?))
}

pub async fn run(path: &str) -> Result<String> {
    match path {
        "/reuse" | "/reuse-seaorm" => {
            let options = options("/certs/ca.crt")?;
            let pool = POOL.get_or_init(|| {
                PgPoolOptions::new()
                    .max_connections(1)
                    .acquire_timeout(Duration::from_secs(2))
                    .connect_lazy_with(options)
            });
            if path == "/reuse-seaorm" {
                use sea_orm::EntityTrait;
                let db = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
                let absent = seaorm_probe::customer::Entity::find_by_id("not-in-the-fixture")
                    .one(&db)
                    .await?;
                ensure!(absent.is_none(), "unexpected fixture row");
            }
            let (pid, tls): (i32, bool) = sqlx::query_as(
                "SELECT pg_backend_pid(), ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
            )
            .fetch_one(pool)
            .await?;
            ensure!(tls, "pooled connection did not use TLS");
            let instance = crate::INSTANCE.get_or_init(wasip3::random::random::get_random_u64);
            let calls = REUSE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            Ok(serde_json::json!({"instance": instance.to_string(), "pid": pid, "calls": calls, "tls": tls}).to_string())
        }
        "/close-pool" => {
            if let Some(pool) = POOL.get() {
                pool.close().await;
            }
            Ok("closed".into())
        }
        "/tls" | "/tls-bad-ca" => {
            let ca = if path == "/tls-bad-ca" {
                "/certs/wrong-ca.crt"
            } else {
                "/certs/ca.crt"
            };
            let mut conn = PgConnection::connect_with(&options(ca)?).await?;
            let tls: bool =
                sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
                    .fetch_one(&mut conn)
                    .await?;
            ensure!(tls, "connection did not use TLS");
            conn.close().await?;
            Ok("verified TLS".into())
        }
        "/cancel" => cancellation().await,
        _ => anyhow::bail!("unknown hardening probe"),
    }
}

// Timeout bounds are the real P3 monotonic-clock implementation from the adapter.
async fn wait_until_active(observer: &mut PgConnection, pid: i32) -> Result<()> {
    sqlx_core::rt::timeout(Duration::from_secs(3), async {
        loop {
            let active: bool = sqlx::query_scalar(
                "SELECT state = 'active' AND query LIKE '%pg_sleep(10)%' FROM pg_stat_activity WHERE pid = $1"
            ).bind(pid).fetch_one(&mut *observer).await?;
            if active { return Ok::<_, anyhow::Error>(()); }
            sqlx_core::rt::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    Ok(())
}

async fn cancellation() -> Result<String> {
    let opts = options("/certs/ca.crt")?;
    let mut target = PgConnection::connect_with(&opts).await?;
    let mut observer = PgConnection::connect_with(&opts).await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut target)
        .await?;

    // Explicit server cancellation, after independently observing the query in flight.
    let mut tx = target.begin().await?;
    let (query_result, ()) = futures_util::try_join!(
        async {
            Ok::<_, anyhow::Error>(sqlx::query("SELECT pg_sleep(10)").execute(&mut *tx).await)
        },
        async {
            wait_until_active(&mut observer, pid).await?;
            let cancelled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
                .bind(pid)
                .fetch_one(&mut observer)
                .await?;
            ensure!(cancelled, "PostgreSQL rejected cancellation");
            Ok::<_, anyhow::Error>(())
        }
    )?;
    let error = query_result.expect_err("cancelled query unexpectedly succeeded");
    ensure!(
        error.as_database_error().and_then(|e| e.code()).as_deref() == Some("57014"),
        "expected query_cancelled, got {error}"
    );
    tx.rollback().await?;
    let recovered: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut target)
        .await?;
    ensure!(recovered == pid, "recovery replaced the connection");

    // Dropping a future is NOT a PostgreSQL cancellation request. Observe this,
    // explicitly cancel server work, and discard the abandoned connection.
    let (timed_out, ()) = futures_util::try_join!(
        async {
            Ok::<_, anyhow::Error>(
                sqlx_core::rt::timeout(
                    Duration::from_millis(250),
                    sqlx::query("SELECT pg_sleep(10)").execute(&mut target),
                )
                .await
                .is_err(),
            )
        },
        async {
            wait_until_active(&mut observer, pid).await?;
            sqlx_core::rt::sleep(Duration::from_millis(500)).await;
            let still_running: bool =
                sqlx::query_scalar("SELECT state = 'active' FROM pg_stat_activity WHERE pid = $1")
                    .bind(pid)
                    .fetch_one(&mut observer)
                    .await?;
            ensure!(
                still_running,
                "expected dropped future to leave server query running"
            );
            let cancelled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
                .bind(pid)
                .fetch_one(&mut observer)
                .await?;
            ensure!(cancelled, "cleanup cancellation failed");
            Ok::<_, anyhow::Error>(())
        }
    )?;
    ensure!(timed_out, "client deadline did not fire");
    target.close_hard().await?;
    let mut replacement = PgConnection::connect_with(&opts).await?;
    let value: i32 = sqlx::query_scalar("SELECT 42")
        .fetch_one(&mut replacement)
        .await?;
    ensure!(value == 42, "replacement connection unusable");
    replacement.close().await?;
    observer.close().await?;
    Ok(serde_json::json!({
        "server_cancel_sqlstate": "57014",
        "same_connection_recovered": true,
        "client_timeout_fired": true,
        "dropped_future_did_not_cancel_server": true,
        "discard_and_reconnect": true
    })
    .to_string())
}
