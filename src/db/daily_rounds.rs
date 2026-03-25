use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyRound {
    pub id: String,
    pub asset_id: String,
    pub asset_external_id: String,
    pub status: String,
    pub data: String,
    pub response: String,
    pub time: String,
}

/// Create a new daily round record.
pub async fn create_daily_round(
    db: &SqlitePool,
    asset_id: &str,
    asset_external_id: &str,
    status: &str,
    data: &str,
) -> Result<DailyRound, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO daily_rounds (id, asset_id, asset_external_id, status, data, response, time) VALUES (?, ?, ?, ?, ?, '', ?)"
    )
    .bind(&id)
    .bind(asset_id)
    .bind(asset_external_id)
    .bind(status)
    .bind(data)
    .bind(&now)
    .execute(db)
    .await?;

    Ok(DailyRound {
        id,
        asset_id: asset_id.to_string(),
        asset_external_id: asset_external_id.to_string(),
        status: status.to_string(),
        data: data.to_string(),
        response: String::new(),
        time: now,
    })
}

/// Update a daily round's response.
pub async fn update_daily_round_response(
    db: &SqlitePool,
    id: &str,
    response: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE daily_rounds SET response = ? WHERE id = ?")
        .bind(response)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// List daily rounds for a given asset external ID.
pub async fn list_daily_rounds(
    db: &SqlitePool,
    asset_external_id: &str,
) -> Result<Vec<DailyRound>, AppError> {
    let rows = sqlx::query(
        "SELECT id, asset_id, asset_external_id, status, data, response, time FROM daily_rounds WHERE asset_external_id = ? ORDER BY time DESC"
    )
    .bind(asset_external_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .iter()
        .map(|row| DailyRound {
            id: row.get("id"),
            asset_id: row.get("asset_id"),
            asset_external_id: row.get("asset_external_id"),
            status: row.get("status"),
            data: row.get("data"),
            response: row.get("response"),
            time: row.get("time"),
        })
        .collect())
}
