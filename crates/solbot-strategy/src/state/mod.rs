pub mod positions_repo;
pub mod state_transitions_repo;
pub mod wallet_watchlist_repo;
pub mod scam_blacklist_repo;
pub mod smart_money_hits_repo;

use sqlx::SqlitePool;
use anyhow::Result;

pub struct DbContext {
    pub pool: SqlitePool,
}

impl DbContext {
    pub async fn init(database_url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(database_url).await?;
        
        // Run migrations
        sqlx::migrate!("../../migrations").run(&pool).await?;

        Ok(Self { pool })
    }
}
