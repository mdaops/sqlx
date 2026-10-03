use anyhow::{Result, ensure};
use sqlx::{FromRow, postgres::PgPoolOptions};

#[derive(Debug, PartialEq, FromRow)]
struct Customer {
    id: String,
    display_name: String,
    note: Option<String>,
}

pub async fn run(url: &str) -> Result<()> {
    let pool = PgPoolOptions::new().max_connections(2).connect(url).await?;
    let mut tx = pool.begin().await?;
    let expected = Customer {
        id: "sqlx".into(),
        display_name: "O'Brien".into(),
        note: None,
    };
    let created = sqlx::query_as::<_, Customer>(
        "INSERT INTO customers (id, display_name, note) VALUES ($1, $2, $3) RETURNING id, display_name, note",
    )
    .bind(&expected.id)
    .bind(&expected.display_name)
    .bind(&expected.note)
    .fetch_one(&mut *tx)
    .await?;
    let found =
        sqlx::query_as::<_, Customer>("SELECT id, display_name, note FROM customers WHERE id = $1")
            .bind(&expected.id)
            .fetch_one(&mut *tx)
            .await?;
    ensure!(
        created == expected && found == expected,
        "typed row mismatch"
    );
    tx.rollback().await?;
    let absent =
        sqlx::query_as::<_, Customer>("SELECT id, display_name, note FROM customers WHERE id = $1")
            .bind(&expected.id)
            .fetch_optional(&pool)
            .await?;
    ensure!(absent.is_none(), "rollback left a row");
    pool.close().await;
    pool_checks(url).await?;
    Ok(())
}

/// Prove reuse, queued concurrent queries, and an actual acquisition timeout.
async fn pool_checks(url: &str) -> Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_millis(100))
        .connect(url)
        .await?;
    let mut held = pool.acquire().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await?;
    ensure!(
        matches!(pool.acquire().await, Err(sqlx::Error::PoolTimedOut)),
        "acquisition did not time out while the only connection was held"
    );
    drop(held);

    let (first, second): (i32, i32) = futures_util::try_join!(
        sqlx::query_scalar("SELECT pg_backend_pid() FROM pg_sleep(0.02)").fetch_one(&pool),
        sqlx::query_scalar("SELECT pg_backend_pid() FROM pg_sleep(0.02)").fetch_one(&pool),
    )?;
    ensure!(
        first == pid && second == pid,
        "pool failed to reuse its connection"
    );
    pool.close().await;
    Ok(())
}
