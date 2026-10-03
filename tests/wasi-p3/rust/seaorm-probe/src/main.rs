#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    seaorm_probe::run(&std::env::var("DATABASE_URL")?).await?;
    println!("PASS SeaORM: entity insert/get, bound parameters, NULL, rollback");
    Ok(())
}
