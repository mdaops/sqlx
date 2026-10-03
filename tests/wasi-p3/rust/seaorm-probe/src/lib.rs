use anyhow::{Result, ensure};
use sea_orm::{ActiveModelTrait, Database, EntityTrait, Set, TransactionTrait};

pub mod customer {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
    #[sea_orm(table_name = "customers")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub display_name: String,
        pub note: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub async fn run(url: &str) -> Result<()> {
    let db = Database::connect(url).await?;
    let tx = db.begin().await?;
    let created = customer::ActiveModel {
        id: Set("seaorm".into()),
        display_name: Set("O'Brien".into()),
        note: Set(None),
    }
    .insert(&tx)
    .await?;
    let found = customer::Entity::find_by_id("seaorm").one(&tx).await?;
    ensure!(
        created.id == "seaorm" && created.display_name == "O'Brien" && created.note.is_none(),
        "typed row mismatch"
    );
    ensure!(found.as_ref() == Some(&created), "entity lookup mismatch");
    tx.rollback().await?;
    ensure!(
        customer::Entity::find_by_id("seaorm")
            .one(&db)
            .await?
            .is_none(),
        "rollback left a row"
    );
    db.close().await?;
    Ok(())
}
