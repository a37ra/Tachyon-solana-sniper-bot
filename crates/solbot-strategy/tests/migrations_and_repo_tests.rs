use anyhow::Result;
use solbot_common::types::position::{Position, PositionStatus};
use solbot_common::types::tier::CapitalTier;
use solbot_strategy::state::positions_repo::PositionsRepo;
use solbot_strategy::state::smart_money_hits_repo::SmartMoneyHitsRepo;
use sqlx::{Row, SqlitePool};

#[tokio::test]
async fn test_positions_closed_at_utc_nullable_after_migrations() -> Result<()> {
    let pool = SqlitePool::connect("sqlite::memory:").await?;
    let migrations_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    for migration in [
        "0001_core_schema.sql",
        "0002_align_with_tz_v1.sql",
        "0003_fix_closed_at_utc_nullable.sql",
    ] {
        let sql = std::fs::read_to_string(migrations_dir.join(migration))?;
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            sqlx::query(statement).execute(&pool).await?;
        }
    }

    let repo = PositionsRepo::new(pool.clone());
    let test_position = Position {
        position_id: "pos-migration-1".to_string(),
        mint: "Mint11111111111111111111111111111111111111".to_string(),
        pool_address: "Pool11111111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        tier: CapitalTier::Micro,
        status: PositionStatus::Open,
        entry_price_sol: 1.0,
        size_sol: 0.5,
        size_pct_of_capital: 0.05,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: 0.5,
        trailing_active: false,
        trailing_high_water_sol: None,
        realized_pnl_net_sol: 0.0,
        opened_at_utc: 1000000,
        closed_at_utc: None,
    };

    repo.insert(&test_position).await?;

    let row = sqlx::query("SELECT closed_at_utc FROM positions WHERE position_id = ?")
        .bind(&test_position.position_id)
        .fetch_one(&pool)
        .await?;
    assert!(row.get::<Option<i64>, _>("closed_at_utc").is_none());

    let table_info = sqlx::query("PRAGMA table_info('positions')")
        .fetch_all(&pool)
        .await?;
    let closed_at_utc_info = table_info
        .into_iter()
        .find(|row| row.get::<String, _>("name") == "closed_at_utc")
        .expect("closed_at_utc column must exist");
    assert_eq!(closed_at_utc_info.get::<i64, _>("notnull"), 0);

    let fk_rows = sqlx::query("PRAGMA foreign_key_list('state_transitions')")
        .fetch_all(&pool)
        .await?;
    assert!(fk_rows.into_iter().any(|row| {
        row.get::<String, _>("table") == "positions" && row.get::<String, _>("to") == "position_id"
    }));

    Ok(())
}

#[tokio::test]
async fn test_smart_money_hits_repo_records_pool_address() -> Result<()> {
    let pool = SqlitePool::connect("sqlite::memory:").await?;
    let migrations_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    for migration in [
        "0001_core_schema.sql",
        "0002_align_with_tz_v1.sql",
        "0003_fix_closed_at_utc_nullable.sql",
    ] {
        let sql = std::fs::read_to_string(migrations_dir.join(migration))?;
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            sqlx::query(statement).execute(&pool).await?;
        }
    }

    let repo = SmartMoneyHitsRepo::new(pool.clone());
    sqlx::query(
        "INSERT INTO wallet_watchlist (wallet_address, label, status, checklist_items_passed, added_at_utc, last_reviewed_at_utc) VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind("wallet1")
    .bind("Test wallet")
    .bind("active")
    .bind(0)
    .bind("2025-01-01T00:00:00Z")
    .bind(0i64)
    .execute(&pool)
    .await?;

    repo.insert_hit(
        "hit-1",
        "wallet1",
        "Mint11111111111111111111111111111111111111",
        "Pool11111111111111111111111111111111111111",
        "txsig1",
        12345,
    )
    .await?;

    let row = sqlx::query(
        "SELECT wallet_address, mint, pool_address, tx_signature, detected_at_slot FROM smart_money_hits WHERE id = ?",
    )
    .bind("hit-1")
    .fetch_one(&pool)
    .await?;

    assert_eq!(row.get::<String, _>("wallet_address"), "wallet1");
    assert_eq!(
        row.get::<String, _>("mint"),
        "Mint11111111111111111111111111111111111111"
    );
    assert_eq!(
        row.get::<String, _>("pool_address"),
        "Pool11111111111111111111111111111111111111"
    );
    assert_eq!(row.get::<String, _>("tx_signature"), "txsig1");
    assert_eq!(row.get::<i64, _>("detected_at_slot"), 12345);

    Ok(())
}
