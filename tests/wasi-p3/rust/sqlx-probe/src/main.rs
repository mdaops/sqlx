#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    sqlx_probe::run(&std::env::var("DATABASE_URL")?).await?;
    println!("PASS SQLx: FromRow insert/get, bound parameters, NULL, rollback");
    Ok(())
}
