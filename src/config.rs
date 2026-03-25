/// Application settings loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Settings {
    pub bind_host: String,
    pub bind_port: u16,
    pub database_url: String,
    pub care_api: String,
    pub care_api_timeout_secs: u64,
    pub gateway_device_id: String,
    pub jwks_base64: Option<String>,
    pub host_name: String,
    pub automated_observations_enabled: bool,
    pub automated_observations_interval_mins: u64,
    pub camera_lock_timeout_secs: u64,
    pub s3_access_key_id: Option<String>,
    pub s3_secret_access_key: Option<String>,
    pub s3_endpoint_url: Option<String>,
    pub s3_bucket_name: Option<String>,
    pub rtsptoweb_url: String,
    pub onvif_accept_invalid_certs: bool,
    pub state_dir: String,
    pub sentry_dsn: Option<String>,
    pub app_version: String,
    pub encryption_key: Option<String>,
}

impl Settings {
    /// Load settings from environment variables (.env file loaded by dotenvy in main).
    pub fn from_env() -> Result<Self, anyhow::Error> {
        let gateway_device_id = std::env::var("GATEWAY_DEVICE_ID").unwrap_or_default();

        let automated_observations_enabled = std::env::var("AUTOMATED_OBSERVATIONS_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or_else(|_| !gateway_device_id.is_empty());

        Ok(Self {
            bind_host: std::env::var("BIND_HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            bind_port: std::env::var("BIND_PORT")
                .unwrap_or_else(|_| "8090".into())
                .parse()?,
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:./gateway.db".into()),
            care_api: std::env::var("CARE_API")
                .unwrap_or_else(|_| "https://care.10bedicu.in".into()),
            care_api_timeout_secs: std::env::var("CARE_API_TIMEOUT_SECS")
                .unwrap_or_else(|_| "25".into())
                .parse()?,
            gateway_device_id,
            jwks_base64: std::env::var("JWKS_BASE64").ok().filter(|s| !s.is_empty()),
            host_name: std::env::var("HOST_NAME").unwrap_or_else(|_| "gateway".into()),
            automated_observations_enabled,
            automated_observations_interval_mins: std::env::var("AUTOMATED_OBSERVATIONS_INTERVAL_MINS")
                .unwrap_or_else(|_| "60".into())
                .parse()?,
            camera_lock_timeout_secs: std::env::var("CAMERA_LOCK_TIMEOUT_SECS")
                .unwrap_or_else(|_| "120".into())
                .parse()?,
            s3_access_key_id: std::env::var("S3_ACCESS_KEY_ID").ok().filter(|s| !s.is_empty()),
            s3_secret_access_key: std::env::var("S3_SECRET_ACCESS_KEY").ok().filter(|s| !s.is_empty()),
            s3_endpoint_url: std::env::var("S3_ENDPOINT_URL").ok().filter(|s| !s.is_empty()),
            s3_bucket_name: std::env::var("S3_BUCKET_NAME").ok().filter(|s| !s.is_empty()),
            rtsptoweb_url: std::env::var("RTSPTOWEB_URL")
                .unwrap_or_else(|_| "http://localhost:8080".into()),
            onvif_accept_invalid_certs: std::env::var("ONVIF_ACCEPT_INVALID_CERTS")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            state_dir: std::env::var("STATE_DIR").unwrap_or_else(|_| "./data".into()),
            sentry_dsn: std::env::var("SENTRY_DSN").ok().filter(|s| !s.is_empty()),
            app_version: std::env::var("APP_VERSION")
                .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into()),
            encryption_key: std::env::var("ENCRYPTION_KEY").ok().filter(|s| !s.is_empty()),
        })
    }

    /// Check whether S3 is fully configured.
    pub fn s3_configured(&self) -> bool {
        self.s3_access_key_id.is_some()
            && self.s3_secret_access_key.is_some()
            && self.s3_bucket_name.is_some()
    }
}
