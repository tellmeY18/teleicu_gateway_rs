mod api;
mod auth;
mod care_client;
mod config;
mod db;
mod error;
mod observations;
mod onvif;
mod state;
mod tasks;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{any, get, post};
use axum::Router;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::auth::outbound::OwnKeypair;
use crate::config::Settings;
use crate::observations::store::ObservationStore;
use crate::onvif::lock::CameraLockMap;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file (ignore errors if not present)
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("[boot] Loaded .env from {}", path.display()),
        Err(e) => eprintln!("[boot] No .env file loaded: {e}"),
    }

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,teleicu_gateway=debug".into()),
        )
        .init();

    // Load settings
    let settings = Settings::from_env()?;

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<error: {e}>"));

    tracing::info!(
        "Starting TeleICU Gateway v{} on {}:{}",
        settings.app_version,
        settings.bind_host,
        settings.bind_port
    );
    tracing::info!("  cwd            = {cwd}");
    tracing::info!("  DATABASE_URL   = {}", settings.database_url);
    tracing::info!("  STATE_DIR      = {}", settings.state_dir);
    tracing::info!("  CARE_API       = {}", settings.care_api);
    tracing::info!("  RTSPTOWEB_URL  = {}", settings.rtsptoweb_url);
    tracing::info!("  GATEWAY_DEVICE_ID = {}", if settings.gateway_device_id.is_empty() { "<not set>" } else { &settings.gateway_device_id });
    tracing::info!("  JWKS_BASE64    = {}", if settings.jwks_base64.is_some() { "<set>" } else { "<not set>" });
    tracing::info!("  S3 configured  = {}", settings.s3_configured());
    tracing::info!("  SENTRY_DSN     = {}", if settings.sentry_dsn.is_some() { "<set>" } else { "<not set>" });
    tracing::info!("  auto_obs       = {} (interval {}m)", settings.automated_observations_enabled, settings.automated_observations_interval_mins);

    // Resolve the database file path from the URL for pre-flight checks
    let db_file_path = settings.database_url
        .strip_prefix("sqlite:")
        .unwrap_or(&settings.database_url);
    let db_path = std::path::Path::new(db_file_path);
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            tracing::warn!(
                "Database parent directory does not exist: {} (resolved from cwd {cwd})",
                parent.display()
            );
            tracing::info!("Creating database directory: {}", parent.display());
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create database directory {}: {e}",
                    parent.display()
                )
            })?;
        }
    }
    tracing::debug!("Database file path resolved to: {}", db_path.display());

    // Check state_dir
    let state_path = std::path::Path::new(&settings.state_dir);
    if !state_path.exists() {
        tracing::info!("STATE_DIR does not exist, creating: {}", state_path.display());
        std::fs::create_dir_all(state_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create STATE_DIR {}: {e}",
                state_path.display()
            )
        })?;
    }

    // Initialize Sentry
    let _sentry_guard = settings.sentry_dsn.as_ref().map(|dsn| {
        tracing::info!("Initializing Sentry");
        sentry::init((
            dsn.as_str(),
            sentry::ClientOptions {
                release: Some(settings.app_version.clone().into()),
                traces_sample_rate: 0.1,
                ..Default::default()
            },
        ))
    });

    // Connect to SQLite
    tracing::info!("Connecting to database: {}", settings.database_url);
    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&settings.database_url)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to open database '{}' (cwd: {cwd}): {e}",
                settings.database_url,
            )
        })?;
    tracing::info!("Database connection established");

    // Run migrations
    sqlx::migrate!("./migrations").run(&db).await?;
    tracing::info!("Database migrations applied");

    // Load or generate RSA keypair
    let own_keypair = OwnKeypair::load_or_generate(
        &settings.state_dir,
        settings.jwks_base64.as_deref(),
    )?;
    tracing::info!("RSA keypair loaded");

    // Build shared HTTP client
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(settings.care_api_timeout_secs))
        .danger_accept_invalid_certs(settings.onvif_accept_invalid_certs)
        .build()?;

    let bind_addr: SocketAddr = format!("{}:{}", settings.bind_host, settings.bind_port).parse()?;

    // Build app state
    let state = AppState {
        db,
        settings: Arc::new(settings.clone()),
        http: http.clone(),
        obs_store: Arc::new(ObservationStore::new()),
        camera_locks: Arc::new(CameraLockMap::new(settings.camera_lock_timeout_secs)),
        care_jwks_cache: Arc::new(RwLock::new(None)),
        own_keypair: Arc::new(own_keypair),
    };

    // Spawn background tasks
    tasks::spawn_all(state.clone());

    // Build router
    let rtsptoweb_url = settings.rtsptoweb_url.clone();
    let app = Router::new()
        // No auth
        .route("/openid-configuration/", get(api::openid::openid_configuration))
        .route("/healthz", get(api::health::healthz))
        .route("/health/ping", get(api::health::ping))
        .route("/health/status", get(api::health::status))
        .route("/health/care/communication", get(api::health::care_communication))
        .route("/health/care/communication-asset", get(api::health::care_communication_asset))
        // LAN-only (IP check, no JWT)
        .route("/update_observations", post(api::observation::update_observations))
        // Care_Bearer required
        .route("/devices/status", get(api::observation::device_status))
        .route("/cameras/status", get(api::camera::get_camera_status))
        .route("/cameras/presets", get(api::camera::get_presets))
        .route("/cameras/absoluteMove", post(api::camera::absolute_move))
        .route("/cameras/relativeMove", post(api::camera::relative_move))
        .route("/cameras/snapshotAtLocation", post(api::camera::snapshot_at_location))
        .route("/cameras/gotoPreset", post(api::camera::goto_preset))
        .route("/cameras/set_preset", post(api::camera::set_preset))
        // Stream tokens
        .route("/getToken/videoFeed", post(api::stream::get_video_feed_token))
        .route("/getToken/vitals", post(api::stream::get_vitals_token))
        .route("/verifyToken", post(api::stream::verify_token))
        .route("/verify_token", post(api::stream::exchange_token))
        // WebSocket
        .route("/observations/{ip_address}", get(ws::observations::ws_observations))
        .route("/logger", get(ws::logger::ws_logger))
        // Reverse proxy to rtsptoweb
        .route("/start", any(move |req: axum::extract::Request| proxy_to_rtsptoweb(req, rtsptoweb_url.clone())))
        .route("/stream", any({
            let url = settings.rtsptoweb_url.clone();
            move |req: axum::extract::Request| proxy_to_rtsptoweb(req, url.clone())
        }))
        .route("/list", any({
            let url = settings.rtsptoweb_url.clone();
            move |req: axum::extract::Request| proxy_to_rtsptoweb(req, url.clone())
        }))
        .route("/stop", any({
            let url = settings.rtsptoweb_url.clone();
            move |req: axum::extract::Request| proxy_to_rtsptoweb(req, url.clone())
        }))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    tracing::info!("Listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Reverse proxy handler for rtsptoweb routes.
async fn proxy_to_rtsptoweb(
    req: axum::extract::Request,
    rtsptoweb_base: String,
) -> Result<axum::response::Response, crate::error::AppError> {
    let path = req.uri().path();
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!(
        "{}{}{}",
        rtsptoweb_base.trim_end_matches('/'),
        path,
        query
    );

    let method = req.method().clone();
    let headers = req.headers().clone();
    let body = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .map_err(|e| crate::error::AppError::Internal(anyhow::anyhow!("body read error: {e}")))?;

    let client = reqwest::Client::new();
    let mut upstream_req = client.request(method, &url);
    for (key, value) in headers.iter() {
        if key == "host" {
            continue;
        }
        upstream_req = upstream_req.header(key.as_str(), value.as_bytes());
    }
    upstream_req = upstream_req.body(body);

    let upstream_resp = upstream_req.send().await.map_err(|e| {
        crate::error::AppError::Internal(anyhow::anyhow!("proxy error: {e}"))
    })?;

    let status = axum::http::StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    let mut builder = axum::response::Response::builder().status(status);

    for (key, value) in upstream_resp.headers().iter() {
        builder = builder.header(key.as_str(), value.as_bytes());
    }

    let resp_body = upstream_resp
        .bytes()
        .await
        .map_err(|e| crate::error::AppError::Internal(anyhow::anyhow!("proxy body error: {e}")))?;

    builder
        .body(axum::body::Body::from(resp_body))
        .map_err(|e| crate::error::AppError::Internal(anyhow::anyhow!("response build error: {e}")))
}
