use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::error::AppError;

/// Asset types matching the database CHECK constraint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AssetType {
    ONVIF,
    HL7MONITOR,
    VENTILATOR,
}

impl std::fmt::Display for AssetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetType::ONVIF => write!(f, "ONVIF"),
            AssetType::HL7MONITOR => write!(f, "HL7MONITOR"),
            AssetType::VENTILATOR => write!(f, "VENTILATOR"),
        }
    }
}

impl std::str::FromStr for AssetType {
    type Err = AppError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ONVIF" => Ok(AssetType::ONVIF),
            "HL7MONITOR" => Ok(AssetType::HL7MONITOR),
            "VENTILATOR" => Ok(AssetType::VENTILATOR),
            _ => Err(AppError::Internal(anyhow::anyhow!(
                "unknown asset type: {s}"
            ))),
        }
    }
}

/// An asset row from the database.
#[derive(Debug, Clone, Serialize)]
pub struct Asset {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub asset_type: String,
    pub description: String,
    pub ip_address: String,
    pub port: i64,
    pub username: Option<String>,
    #[serde(skip_serializing)]
    pub password_enc: Option<Vec<u8>>,
    pub access_key: Option<String>,
    pub deleted: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Request body for creating/updating an asset.
#[derive(Debug, Deserialize)]
pub struct AssetInput {
    pub name: String,
    #[serde(rename = "type")]
    pub asset_type: String,
    #[serde(default)]
    pub description: String,
    pub ip_address: String,
    #[serde(default = "default_port")]
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub access_key: Option<String>,
}

fn default_port() -> i64 {
    80
}

/// Decode a hex-encoded string into bytes without requiring an external `hex` crate.
fn hex_decode(s: &str) -> Result<Vec<u8>, AppError> {
    if s.len() % 2 != 0 {
        return Err(AppError::Internal(anyhow::anyhow!(
            "invalid hex string length"
        )));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("invalid hex: {e}")))
        })
        .collect()
}

/// Encrypt a password using AES-GCM-256. Returns IV ∥ ciphertext.
pub fn encrypt_password(password: &str, encryption_key: &str) -> Result<Vec<u8>, AppError> {
    let key_bytes = hex_decode(encryption_key)?;
    if key_bytes.len() != 32 {
        return Err(AppError::Internal(anyhow::anyhow!(
            "ENCRYPTION_KEY must be 32 bytes (64 hex chars)"
        )));
    }

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("AES key error: {e}")))?;

    let mut iv = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut iv);
    let nonce = Nonce::from_slice(&iv);

    let ciphertext = cipher
        .encrypt(nonce, password.as_bytes())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("encryption failed: {e}")))?;

    // Prepend IV to ciphertext
    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&iv);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Decrypt a password from IV ∥ ciphertext.
pub fn decrypt_password(encrypted: &[u8], encryption_key: &str) -> Result<String, AppError> {
    if encrypted.len() < 13 {
        return Err(AppError::Internal(anyhow::anyhow!(
            "encrypted data too short"
        )));
    }

    let key_bytes = hex_decode(encryption_key)?;

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("AES key error: {e}")))?;

    let (iv, ciphertext) = encrypted.split_at(12);
    let nonce = Nonce::from_slice(iv);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("decryption failed: {e}")))?;

    String::from_utf8(plaintext)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("invalid UTF-8 in password: {e}")))
}

/// Map a `sqlx::sqlite::SqliteRow` into an `Asset`.
fn row_to_asset(row: &sqlx::sqlite::SqliteRow) -> Asset {
    Asset {
        id: row.get("id"),
        name: row.get("name"),
        asset_type: row.get("type"),
        description: row.get("description"),
        ip_address: row.get("ip_address"),
        port: row.get("port"),
        username: row.get("username"),
        password_enc: row.get("password_enc"),
        access_key: row.get("access_key"),
        deleted: row.get::<bool, _>("deleted"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

/// List all non-deleted assets, optionally filtered by type.
pub async fn list_assets(
    db: &SqlitePool,
    asset_type: Option<&str>,
) -> Result<Vec<Asset>, AppError> {
    let rows = if let Some(t) = asset_type {
        sqlx::query(
            "SELECT id, name, type, description, ip_address, port, \
                    username, password_enc, access_key, deleted, created_at, updated_at \
             FROM assets WHERE deleted = 0 AND type = ? ORDER BY created_at DESC",
        )
        .bind(t)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query(
            "SELECT id, name, type, description, ip_address, port, \
                    username, password_enc, access_key, deleted, created_at, updated_at \
             FROM assets WHERE deleted = 0 ORDER BY created_at DESC",
        )
        .fetch_all(db)
        .await?
    };

    Ok(rows.iter().map(row_to_asset).collect())
}

/// Get a single asset by ID.
pub async fn get_asset(db: &SqlitePool, id: &str) -> Result<Asset, AppError> {
    let row = sqlx::query(
        "SELECT id, name, type, description, ip_address, port, \
                username, password_enc, access_key, deleted, created_at, updated_at \
         FROM assets WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(row_to_asset(&row))
}

/// Create a new asset.
pub async fn create_asset(
    db: &SqlitePool,
    input: &AssetInput,
    encryption_key: Option<&str>,
) -> Result<Asset, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let password_enc = match (&input.password, encryption_key) {
        (Some(pwd), Some(key)) => Some(encrypt_password(pwd, key)?),
        _ => None,
    };

    sqlx::query(
        "INSERT INTO assets (id, name, type, description, ip_address, port, \
                             username, password_enc, access_key, deleted, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(&input.name)
    .bind(&input.asset_type)
    .bind(&input.description)
    .bind(&input.ip_address)
    .bind(input.port)
    .bind(&input.username)
    .bind(&password_enc)
    .bind(&input.access_key)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;

    get_asset(db, &id).await
}

/// Soft-delete an asset.
pub async fn delete_asset(db: &SqlitePool, id: &str) -> Result<(), AppError> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE assets SET deleted = 1, updated_at = ? WHERE id = ? AND deleted = 0",
    )
    .bind(&now)
    .bind(id)
    .execute(db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}
