# TeleICU Gateway — Rust Implementation Spec

## Project Layout

```
teleicu-gateway/
├── Cargo.toml
├── Cargo.lock
├── .env.example
├── flake.nix
├── migrations/
│   └── 0001_initial.sql
└── src/
    ├── main.rs
    ├── config.rs
    ├── error.rs
    ├── state.rs
    ├── db/
    │   ├── mod.rs
    │   ├── assets.rs
    │   └── daily_rounds.rs
    ├── auth/
    │   ├── mod.rs
    │   ├── inbound.rs      # validate Care_Bearer JWTs from CARE
    │   └── outbound.rs     # sign Gateway_Bearer JWTs, manage own keypair
    ├── api/
    │   ├── mod.rs
    │   ├── health.rs
    │   ├── camera.rs
    │   ├── observation.rs
    │   ├── stream.rs
    │   └── openid.rs
    ├── onvif/
    │   ├── mod.rs
    │   ├── soap.rs         # envelope builder + WS-Security header
    │   ├── client.rs       # OnvifClient — all operations
    │   └── lock.rs         # per-camera async mutex map
    ├── observations/
    │   ├── mod.rs
    │   ├── types.rs        # Observation enum + all sub-types
    │   ├── store.rs        # in-memory ring buffer per device
    │   └── validity.rs     # is_valid() logic
    ├── tasks/
    │   ├── mod.rs
    │   ├── automated_observations.rs
    │   ├── camera_status.rs
    │   └── s3_dump.rs
    ├── ws/
    │   ├── mod.rs
    │   ├── observations.rs # ws://.../observations/<ip>
    │   └── logger.rs       # ws://.../logger — system metrics
    └── care_client.rs      # typed CARE API client
```

---

## Cargo.toml Dependencies

```toml
[package]
name = "teleicu-gateway"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "teleicu-gateway"
path = "src/main.rs"

[dependencies]
# Async runtime
tokio          = { version = "1", features = ["full"] }

# Web framework
axum           = { version = "0.7", features = ["ws", "macros"] }
tower          = "0.4"
tower-http     = { version = "0.5", features = ["cors", "trace", "limit"] }

# Serialization
serde          = { version = "1", features = ["derive"] }
serde_json     = "1"

# Database
sqlx           = { version = "0.7", features = ["runtime-tokio-rustls", "sqlite", "migrate", "uuid", "chrono"] }

# HTTP client
reqwest        = { version = "0.12", features = ["json", "rustls-tls", "stream"], default-features = false }

# JWT — inbound validation
jsonwebtoken   = "9"

# RSA keypair — outbound signing + JWKS exposure
rsa            = { version = "0.9", features = ["pem", "sha2"] }
pkcs8          = { version = "0.10", features = ["alloc", "pem"] }

# XML — ONVIF SOAP
quick-xml      = { version = "0.31", features = ["serialize"] }

# Crypto — ONVIF WS-Security digest
sha1           = "0.10"
base64         = "0.22"

# Credential encryption at rest
aes-gcm        = "0.10"
rand           = "0.8"

# Config
dotenvy        = "0.15"

# Time, IDs, logging
uuid           = { version = "1", features = ["v4", "serde"] }
chrono         = { version = "0.4", features = ["serde"] }
tracing        = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

# System metrics — LoggerConsumer
sysinfo        = "0.30"

# S3
aws-config     = "1"
aws-sdk-s3     = "1"

# Error handling
anyhow         = "1"
thiserror      = "1"

# Async trait (required by axum-core 0.4.x's FromRequestParts)
async-trait    = "0.1"

# Concurrent map for camera locks and observation store
dashmap        = "5"

# Sentry
sentry         = { version = "0.34", features = ["tracing"] }
```

> **Dep drift note**: The Cargo.toml in this doc is a reference snapshot. Always treat the real `Cargo.toml` as source of truth — check it before adding or changing dependencies. Notable past corrections: `reqwest` is 0.12 (not 0.11), `p256` and `config` crates were never actually used and have been removed.

---

## Configuration (`src/config.rs`)

Load with the `config` crate, `.env` file merged with environment. All fields required unless default noted.

```rust
pub struct Settings {
    pub bind_host: String,                   // default "0.0.0.0"
    pub bind_port: u16,                      // default 8090

    pub database_url: String,                // "sqlite:./gateway.db?mode=rwc"

    pub care_api: String,                    // "https://care.10bedicu.in"
    pub care_api_timeout_secs: u64,          // default 25
    pub gateway_device_id: String,           // "" means automated obs disabled

    // Base64-encoded JWK keyset (RSA private key) — gateway's own identity.
    // If empty/absent: generate a new keypair on startup and persist to
    // {state_dir}/jwks.json. On subsequent starts, load from that file.
    pub jwks_base64: Option<String>,

    pub host_name: String,                   // used as S3 key prefix

    pub automated_observations_enabled: bool, // default: !gateway_device_id.is_empty()
    pub automated_observations_interval_mins: u64, // default 60

    pub camera_lock_timeout_secs: u64,       // default 120

    pub s3_access_key_id: Option<String>,
    pub s3_secret_access_key: Option<String>,
    pub s3_endpoint_url: Option<String>,
    pub s3_bucket_name: Option<String>,

    pub rtsptoweb_url: String,               // "http://localhost:8080"

    pub onvif_accept_invalid_certs: bool,    // default true
    pub state_dir: String,                   // default "./data" — persisted keypair etc.

    pub sentry_dsn: Option<String>,
    pub app_version: String,                 // default env!("CARGO_PKG_VERSION")
    pub encryption_key: Option<String>,      // AES key for encrypting asset credentials at rest
}
```

**Env var names** (SCREAMING_SNAKE prefix, no prefix needed):
`BIND_HOST`, `BIND_PORT`, `DATABASE_URL`, `CARE_API`, `CARE_API_TIMEOUT_SECS`, `GATEWAY_DEVICE_ID`, `JWKS_BASE64`, `HOST_NAME`, `AUTOMATED_OBSERVATIONS_ENABLED`, `AUTOMATED_OBSERVATIONS_INTERVAL_MINS`, `CAMERA_LOCK_TIMEOUT_SECS`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `S3_ENDPOINT_URL`, `S3_BUCKET_NAME`, `RTSPTOWEB_URL`, `ONVIF_ACCEPT_INVALID_CERTS`, `STATE_DIR`, `SENTRY_DSN`, `APP_VERSION`

---

## Database Schema (`migrations/0001_initial.sql`)

Faithful to the original model, minus Postgres-specific types.

```sql
CREATE TABLE IF NOT EXISTS assets (
    id           TEXT    PRIMARY KEY,          -- UUID v4
    name         TEXT    NOT NULL,
    type         TEXT    NOT NULL CHECK(type IN ('ONVIF','HL7MONITOR','VENTILATOR')),
    description  TEXT    NOT NULL DEFAULT '',
    ip_address   TEXT    NOT NULL,
    port         INTEGER NOT NULL DEFAULT 80,
    username     TEXT,
    password_enc BLOB,                         -- AES-GCM-256 encrypted, IV prepended
    access_key   TEXT,
    deleted      INTEGER NOT NULL DEFAULT 0,   -- soft delete
    created_at   TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS assets_ip ON assets(ip_address);
CREATE INDEX IF NOT EXISTS assets_type ON assets(type, deleted);

CREATE TABLE IF NOT EXISTS daily_rounds (
    id               TEXT    PRIMARY KEY,      -- UUID v4
    asset_id         TEXT    NOT NULL REFERENCES assets(id),
    asset_external_id TEXT   NOT NULL,         -- CARE-side UUID
    status           TEXT    NOT NULL,
    data             TEXT    NOT NULL,         -- JSON
    response         TEXT    NOT NULL DEFAULT '',
    time             TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS daily_rounds_asset ON daily_rounds(asset_external_id);
```

**Credential encryption**: AES-GCM-256. Key derived from `ENCRYPTION_KEY` env var (32 hex bytes). Prepend 12-byte random IV to ciphertext. `password_enc = IV || ciphertext`. Decrypt on read, encrypt on write. Never return raw password in API responses.

> **Improvement over original**: The original stored passwords as plaintext `CharField`. Encrypt at rest.

---

## Error Type (`src/error.rs`)

```rust
#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("camera locked — another operation is in progress")]
    CameraLocked,
    #[error("invalid camera credentials")]
    InvalidCameraCredentials,
    #[error("onvif error: {0}")]
    Onvif(String),
    #[error("care api error: {0}")]
    CareApi(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

// impl IntoResponse: map to appropriate HTTP status + JSON { "error": "..." }
```

---

## App State (`src/state.rs`)

```rust
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub settings: Arc<Settings>,
    pub http: reqwest::Client,                             // shared, keep-alive
    pub obs_store: Arc<ObservationStore>,                  // in-memory observations
    pub camera_locks: Arc<CameraLockMap>,                  // per-IP async mutexes
    pub care_jwks_cache: Arc<tokio::sync::RwLock<Option<CachedJwks>>>,  // inbound key
    pub own_keypair: Arc<OwnKeypair>,                      // gateway's RSA keypair
}
```

All expensive resources (DB pool, HTTP client, keypair) are initialised once in `main.rs` and shared via `Arc`.

---

## Auth (`src/auth/`)

### Two-Direction JWT Trust

**Inbound** (`inbound.rs`) — validate `Care_Bearer <token>` from CARE:

1. Strip `Care_Bearer ` prefix. Return `401` if absent or wrong scheme.
2. Fetch CARE's JWKS from `{CARE_API}/api/gateway_device/jwks.json/`.
3. Cache the key in `care_jwks_cache` for 5 minutes (use `tokio::time::Instant` for TTL — no Redis).
4. Decode with `jsonwebtoken`, algorithm `RS256`.
5. Validate `exp`. Return `ValidatedClaims { sub, ... }` on success.

Implement as an Axum extractor: `struct CareAuth(ValidatedClaims)` with `#[async_trait] impl FromRequestParts`.

**Outbound** (`outbound.rs`) — sign `Gateway_Bearer <token>` to CARE:

1. The gateway's own RSA private key lives in `OwnKeypair`.
2. `generate_gateway_jwt(keypair, claims, exp_secs) -> String` — standard RS256 JWT with `iat`, `exp`, plus any extra claims.
3. Called by `CareClient` on every outbound request.
4. Standard headers: `Authorization: Gateway_Bearer <token>`, `X-Gateway-Id: <GATEWAY_DEVICE_ID>`.

### Own Keypair Bootstrap (`outbound.rs`)

```
fn load_or_generate_keypair(state_dir, jwks_base64_override) -> OwnKeypair:
  if jwks_base64_override is set:
      decode base64 → parse JWK → load RSA private key
  else if {state_dir}/jwks.json exists:
      load from file
  else:
      generate RSA-2048 keypair
      serialize as JWK keyset
      write to {state_dir}/jwks.json
      return keypair
```

Expose `GET /openid-configuration/` returning `{ "keys": [ <public JWK> ] }`. This is how CARE verifies the gateway's outbound tokens. No auth required on this endpoint.

**WebSocket token validation**: tokens passed in `Sec-WebSocket-Protocol` header as `Token, <jwt>`. Validate with the gateway's own public key (same `OwnKeypair`). Reject connection with close code `4000` if invalid.

---

## Observation Store (`src/observations/store.rs`)

Replaces Redis for observation fan-out. Pure in-memory, no broker.

```rust
pub struct ObservationStore {
    // ring buffer per device — bounded to last 2 hours of data
    buffers: DashMap<String, VecDeque<Observation>>,
    // broadcast channel per device for WebSocket push
    channels: DashMap<String, tokio::sync::broadcast::Sender<Vec<Observation>>>,
    // last known device status: "up" | "down"
    device_status: DashMap<String, DeviceStatus>,
}

impl ObservationStore {
    // Called by POST /update_observations
    pub fn ingest(&self, observations: Vec<Observation>)
    // Called by automated_observations task
    pub fn get_static(&self, device_id: &str, since: Duration) -> Option<StaticObservation>
    // Called by WebSocket handler
    pub fn subscribe(&self, device_id: &str) -> broadcast::Receiver<Vec<Observation>>
    // Called by camera_status task / GET /devices/status
    pub fn get_device_statuses(&self) -> HashMap<String, DeviceStatus>
    // Called by s3_dump task
    pub fn drain_stale(&self, older_than: Duration) -> Vec<Observation>
}
```

Blood pressure carry-forward: maintain a `DashMap<device_id, Observation>` for last BP reading and append it to every push that lacks one (replicates `update_blood_pressure` logic).

> **Improvement over original**: Redis key-by-timestamp with string parsing is replaced with a properly typed in-memory structure. No external process needed.

---

## Observation Types (`src/observations/types.rs`)

```rust
pub enum ObservationId {
    HeartRate, ST, SpO2, PulseRate, RespiratoryRate,
    BodyTemperature1, BodyTemperature2, BloodPressure,
    Waveform, DeviceConnection, WaveformII, WaveformPleth, WaveformRespiration,
}

pub struct BloodPressureReading {
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub interpretation: Option<Interpretation>,
    pub low_limit: Option<f64>,
    pub high_limit: Option<f64>,
}

pub struct Observation {
    pub observation_id: ObservationId,
    pub device_id: String,
    pub date_time: DateTime<Utc>,
    pub patient_id: String,
    pub patient_name: Option<String>,
    pub status: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub interpretation: Option<Interpretation>,
    pub low_limit: Option<f64>,
    pub high_limit: Option<f64>,
    pub systolic: Option<BloodPressureReading>,
    pub diastolic: Option<BloodPressureReading>,
    pub map: Option<BloodPressureReading>,
    pub wave_name: Option<WaveName>,
    pub resolution: Option<String>,
    pub sampling_rate: Option<String>,
    pub data_baseline: Option<f64>,
    pub data_low_limit: Option<f64>,
    pub data_high_limit: Option<f64>,
    pub data: Option<String>,
    pub taken_at: DateTime<Utc>,           // set on ingest, not from wire
}
```

Deserialize with `serde` using `#[serde(rename = "date-time")]` / `#[serde(rename = "patient-id")]` etc. to match the exact wire format from monitors.

**Validity** (`validity.rs`): port `is_valid()` exactly — check for sensor-off status strings, null values where required, blood pressure special case.

**FHIR unit codes**: maintain `UNIT_CODES` and `OBSERVATION_ID_CODE_MAPPING` as `phf::Map` or `lazy_static` `HashMap` for zero-overhead lookups when building automated observation payloads.

---

## API Routes

```
# No auth
GET  /openid-configuration/              → { keys: [public JWK] }
GET  /healthz                            → { server, database }
GET  /health/ping                        → { pong: timestamp }
GET  /health/status                      → { server, database }
GET  /health/care/communication          → proxy to CARE /middleware/verify
GET  /health/care/communication-asset    → proxy with asset JWT

# Care_Bearer required for all below
POST /update_observations                → 200 (LAN-only — see security note)
GET  /devices/status                     → [{ time, status: {ip: "up"|"down"} }]

GET  /cameras/status                     → [{ time, status: {ip: "up"|"down"} }]
GET  /cameras/presets?hostname=&port=&username=&password=   → { presets: {name: idx} }
GET  /cameras/status?hostname=&port=&username=&password=    → PTZ status object
POST /cameras/absoluteMove               → 200
POST /cameras/relativeMove               → 200
POST /cameras/snapshotAtLocation         → { status, uri }
POST /cameras/gotoPreset                 → 200 | 404
POST /cameras/set_preset                 → 200

POST /getToken/videoFeed                 → { token }
POST /getToken/vitals                    → { token }
POST /verifyToken                        → { status: "1"|"0" }
POST /verify_token                       → { token }    (exchange CARE token for gateway token)

# WebSocket — token in Sec-WebSocket-Protocol: "Token, <jwt>"
WS   /observations/<ip_address>          → stream of observation JSON arrays
WS   /logger                             → stream of { type, cpu, memory, uptime, load }
```

**Security improvement on `/update_observations`**: In the original, Nginx blocks this with `return 404`. In Rust, add an Axum middleware that rejects requests to this path unless the source IP is loopback or a configured LAN CIDR. This makes the security constraint explicit and not dependent on an external proxy.

---

## Camera API (`src/api/camera.rs`)

**Important**: the camera API is **stateless per-call** — credentials come in every request as query params or POST body. No stored session. This is the existing contract with CARE.

Request type for GET operations:
```rust
#[derive(serde::Deserialize)]
pub struct CameraParams {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}
```

Request type for move operations (POST body):
```rust
#[derive(serde::Deserialize)]
pub struct CameraMoveRequest {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub x: f32,    // pan
    pub y: f32,    // tilt
    pub zoom: f32,
}

pub struct CameraPresetRequest {
    // same credentials +
    pub preset: Option<i32>,
    pub preset_name: Option<String>,  // AliasChoices: "preset" or "presetName"
}
```

Response for PTZ status:
```rust
pub struct PtzStatus {
    pub position: Position,           // { x, y, zoom }
    pub move_status: MoveStatus,      // { pan_tilt, zoom } — "IDLE" | "MOVING"
    pub error: Option<String>,        // None if "noerror" / "NO error"
}
```

---

## ONVIF Client (`src/onvif/`)

### WS-Security (`soap.rs`)

Every SOAP envelope wraps a `<s:Header>` with a `<Security>` block:

```
password_digest = base64( SHA-1( nonce_raw_bytes ++ created_utf8 ++ password_utf8 ) )
nonce = 16 random bytes; nonce_b64 = base64(nonce)
created = UTC timestamp, ISO 8601 format
```

Use the `sha1` crate (not SHA-256, not MD5 — ONVIF spec mandates SHA-1 for `PasswordDigest`).

Build SOAP envelopes as format strings or quick-xml push writers. **Do not** use a WSDL code generator.

Namespaces in use:
- Envelope: `http://www.w3.org/2003/05/soap-envelope`
- Security: `http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd`
- Utility: `http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd`
- PTZ: `http://www.onvif.org/ver20/ptz/wsdl`
- Media: `http://www.onvif.org/ver10/media/wsdl`
- Device: `http://www.onvif.org/ver10/device/wsdl`

### `OnvifClient` (`client.rs`)

```rust
pub struct OnvifClient {
    http: reqwest::Client,   // shared from AppState — not per-instance
    base_url: String,        // http://<hostname>:<port>
    username: String,
    password: String,
}

impl OnvifClient {
    pub async fn get_profiles(&self) -> Result<Vec<Profile>, AppError>
    pub async fn get_status(&self, profile_token: &str) -> Result<PtzStatus, AppError>
    pub async fn get_presets(&self, profile_token: &str) -> Result<Vec<Preset>, AppError>
    pub async fn goto_preset(&self, profile_token: &str, preset_token: &str) -> Result<String, AppError>
    pub async fn set_preset(&self, profile_token: &str, preset_name: &str) -> Result<(), AppError>
    pub async fn absolute_move(&self, profile_token: &str, pan: f32, tilt: f32, zoom: f32) -> Result<(), AppError>
    pub async fn relative_move(&self, profile_token: &str, pan: f32, tilt: f32, zoom: f32) -> Result<(), AppError>
    pub async fn get_snapshot_uri(&self, profile_token: &str) -> Result<String, AppError>
    pub async fn wait_for_idle(&self, profile_token: &str) -> Result<(), AppError>
}
```

ONVIF endpoint paths (relative to base URL):
- Device: `/onvif/device_service`
- Media: `/onvif/media_service`
- PTZ: `/onvif/ptz_service`

> **Do not hardcode paths** — `GetCapabilities` returns the actual service endpoints per camera model. Fetch them on first use and cache in `OnvifClient`.

`wait_for_idle`: async poll loop with `tokio::time::sleep(Duration::from_millis(500))`, timeout after `camera_lock_timeout_secs`. Replaces the blocking `sleep` in the original decorator.

### Camera Lock (`lock.rs`)

```rust
pub struct CameraLockMap {
    locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    timeout: Duration,
}

impl CameraLockMap {
    // Returns Err(AppError::CameraLocked) if lock not acquired within timeout
    pub async fn try_lock(&self, ip: &str) -> Result<LockGuard, AppError>
}
```

> **Improvement over original**: Redis cache-based locking had a TOCTOU race (check then set are not atomic). Per-IP `tokio::sync::Mutex` provides true mutual exclusion within the process.

---

## Background Tasks (`src/tasks/`)

### Spawn All

```rust
pub fn spawn_all(state: AppState) {
    tokio::spawn(automated_observations_loop(state.clone()));
    tokio::spawn(camera_status_loop(state.clone()));
    tokio::spawn(s3_dump_loop(state.clone()));
}
```

### Automated Observations (`automated_observations.rs`)

Interval: `AUTOMATED_OBSERVATIONS_INTERVAL_MINS`. Only runs if `automated_observations_enabled`.

```
1. GET {CARE_API}/api/vitals_observation_device/automated_observations/
   → list of monitors with { id, endpoint_address }
2. For each monitor:
   a. obs_store.get_static(endpoint_address, interval_duration)
   b. get_entries_for_automated_observations(static_obs)  → Vec<ObservationWriteSpec>
   c. POST {CARE_API}/api/vitals_observation_device/automated_observations/{id}/record/
      body: [ObservationWriteSpec as FHIR-ish JSON]
```

`ObservationWriteSpec` shape (for the POST body):
```rust
pub struct ObservationWriteSpec {
    pub status: String,              // "final"
    pub category: Coding,            // VitalSignsCoding constant
    pub main_code: Coding,
    pub effective_datetime: DateTime<Utc>,
    pub value_type: String,          // "integer" | "decimal"
    pub value: ObservationValue,     // { value: String, unit: Coding }
    pub note: Option<String>,
    pub reference_range: Vec<ReferenceRange>,
    pub interpretation: Option<String>,
}
```

Only ship observations from `OBSERVATION_TYPES_FOR_AUTOMATED_OBSERVATIONS`: HeartRate, PulseRate, SpO2, RespiratoryRate, BodyTemperature1, BodyTemperature2, BloodPressure. Staleness cutoff: observations older than `interval_duration` from `last_updated` are skipped.

### Camera Status Sweep (`camera_status.rs`)

Interval: same as automated observations interval (configurable, default 5 min).

```
1. SELECT * FROM assets WHERE type = 'ONVIF' AND deleted = 0
2. For each: OnvifClient::get_status(first profile token)
3. Write { ip: "up" | "down" } to obs_store.device_status (reuse the store)
```

Note: `get_status` returns `PtzStatus.error`. Treat `None` or `"noerror"`/`"NO error"` (case-insensitive, whitespace-stripped) as "up".

### S3 Dump (`s3_dump.rs`)

Interval: every 30 minutes (matches `"30 * * * *"` cron from original).

```
1. obs_store.drain_stale(interval_duration) → Vec<Observation>
2. if empty: return
3. aws_sdk_s3::put_object(
     bucket: S3_BUCKET_NAME,
     key: "{HOST_NAME}/{timestamp}.json",
     body: serde_json::to_vec(&observations)
   )
```

Only run if S3 is configured (all four S3 env vars present). If `S3_ENDPOINT_URL` is set, use it as the custom endpoint (supports S3-compatible stores like MinIO).

---

## Care Client (`src/care_client.rs`)

```rust
pub struct CareClient {
    http: reqwest::Client,
    base_url: String,
    timeout: Duration,
    keypair: Arc<OwnKeypair>,
    gateway_device_id: String,
}

impl CareClient {
    fn headers(&self, extra_claims: Option<Value>) -> HeaderMap  // generates gateway JWT + X-Gateway-Id
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, AppError>
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, AppError>
}
```

Error mapping: 4xx → `AppError::CareApi`, timeout → `AppError::CareApi("timeout")`, conn error → `AppError::CareApi("unreachable")`.

---

## WebSocket Handlers (`src/ws/`)

### Observations (`observations.rs`)

`WS /observations/<ip_address>`

1. Validate `Sec-WebSocket-Protocol` — parse `Token, <jwt>`, validate against own public key. Close `4000` on failure.
2. `let rx = obs_store.subscribe(ip_address)`.
3. Loop: receive from broadcast channel, serialize as JSON array, send to WS client.
4. On disconnect: drop `rx` — broadcast sender is retained by store.

> No auth on the current implementation (commented out). Implement it properly in Rust using the `Token, <jwt>` protocol header pattern.

### Logger (`logger.rs`)

`WS /logger`

No auth in original (plain `self.accept()`). Keep unauthenticated for now (internal use only), but document it.

Loop every 2 seconds:
```rust
let sys = sysinfo::System::new_all();
let payload = serde_json::json!({
    "type": "RESOURCE",
    "cpu":    format!("{:.2}", sys.global_cpu_info().cpu_usage()),
    "memory": format!("{:.2}", sys.used_memory() as f64 / sys.total_memory() as f64 * 100.0),
    "uptime": (SystemTime::now() - UNIX_EPOCH).as_millis() - sys.boot_time() * 1000,
    "load":   format!("{:.2}", sys.load_average().five),
});
```

---

## Stream Token API (`src/api/stream.rs`)

### POST /getToken/videoFeed  (Care_Bearer required)

Request:
```rust
pub struct VideoStreamRequest {
    pub ip: String,
    pub stream: String,
    pub _duration: Option<String>,  // minutes, 1–60, default 5
}
```
Validate duration is 0–60. Sign a gateway JWT with claims `{ stream, ip }`, exp = `duration * 60`. Return `{ "token": "<jwt>" }`.

### POST /getToken/vitals  (Care_Bearer required)

Same but claims `{ asset_id, ip }`.

### POST /verifyToken  (no auth)

Request: `{ token, ip, stream }`. Validate token with own public key, decode claims as `VideoStreamRequest`. Return `{ "status": "1" }` if `ip` or `stream` matches, `{ "status": "0" }` (401) otherwise.

### POST /verify_token  (no auth)

Exchange CARE token for gateway token. Forward `{ token }` to `{CARE_API}/api/v1/auth/token/verify/`. On 200, issue a gateway JWT (20 min expiry). Return `{ "token": "<gateway_jwt>" }`.

---

## Routing Notes (internalising Nginx)

The original Nginx config (port 8001) proxies these paths to `rtsptoweb:8080`: `/start`, `/stream`, `/list`, `/stop`. In the Rust binary, add an Axum route that reverse-proxies these:

```rust
Router::new()
    .route("/start",  any(proxy_to_rtsptoweb))
    .route("/stream", any(proxy_to_rtsptoweb))   // streaming — use chunked pass-through
    .route("/list",   any(proxy_to_rtsptoweb))
    .route("/stop",   any(proxy_to_rtsptoweb))
```

Rate limiting (replaces `limit_req zone=ip burst=12 delay=8` in Nginx): apply `tower_http::limit` or a token-bucket middleware (`tower-governor` crate) to these stream proxy routes. 20 req/s per IP, burst 12.

For `/stream` specifically: pipe response body bytes as a stream without buffering. Use `reqwest`'s streaming response body and `axum::body::StreamBody`.

---

## NixOS Packaging (`flake.nix`)

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
        rtsptoweb = pkgs.fetchurl {
          url = "https://github.com/deepch/RTSPtoWeb/releases/download/v0.0.10/RTSPtoWeb_linux_amd64.tar.gz";
          hash = "sha256-<pin this>";
        };
      in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "teleicu-gateway";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];
        };
      }
    ) // {
      nixosModules.default = { config, pkgs, lib, ... }: {
        options.services.teleicu-gateway = {
          enable = lib.mkEnableOption "TeleICU Gateway";
          environmentFile = lib.mkOption { type = lib.types.path; };
          rtsptowebConfigFile = lib.mkOption { type = lib.types.path; };
        };

        config = lib.mkIf config.services.teleicu-gateway.enable {
          systemd.services.rtsptoweb = {
            description = "RTSPtoWeb stream server";
            wantedBy = [ "multi-user.target" ];
            after = [ "network.target" ];
            serviceConfig = {
              ExecStart = "${rtsptoweb}/RTSPtoWeb --config ${config.services.teleicu-gateway.rtsptowebConfigFile}";
              Restart = "on-failure";
              RestartSec = "3s";
              DynamicUser = true;
            };
          };

          systemd.services.teleicu-gateway = {
            description = "TeleICU Gateway";
            wantedBy = [ "multi-user.target" ];
            after = [ "network.target" "rtsptoweb.service" ];
            serviceConfig = {
              ExecStart = "${pkgs.teleicu-gateway}/bin/teleicu-gateway";
              EnvironmentFile = config.services.teleicu-gateway.environmentFile;
              Restart = "on-failure";
              RestartSec = "5s";
              DynamicUser = true;
              StateDirectory = "teleicu-gateway";     # /var/lib/teleicu-gateway
              WorkingDirectory = "/var/lib/teleicu-gateway";
              # Hardening
              PrivateTmp = true;
              NoNewPrivileges = true;
              ProtectSystem = "strict";
              ProtectHome = true;
              ReadWritePaths = [ "/var/lib/teleicu-gateway" ];
            };
          };
        };
      };
    };
}
```

---

## Implementation Notes & Improvements Over Original

**Passwords in query params**: The original passes camera credentials as URL query params on every request. They appear in access logs. In Rust: accept as before (maintain API compat with CARE) but log requests at `trace` level only, never `info`/`debug` for credential-carrying endpoints. Add a note to operators to use a log level of `info` or above in production.

**`set_preset` name collision check**: The original iterates existing presets and returns `None` if the name already exists. Replicate this check in `OnvifClient::set_preset` before calling the SOAP command.

**`goto_preset` by index**: The original uses integer index into the preset list (not token). `GotoPreset` maps index → `preset.token`. Replicate this exactly — CARE sends a numeric preset index.

**`wait_for_idle` timeout**: The original has no timeout on the polling loop. In Rust, bound it with `tokio::time::timeout(camera_lock_timeout, wait_for_idle(...))`. Return `AppError::Onvif("movement timed out")` on expiry.

**`CameraLockedException` flow**: In the original, locks are checked before move but the check and lock are two separate cache ops (not atomic). The Rust `tokio::sync::Mutex` per camera IP is correct mutual exclusion.

**`DailyRound` model**: Exists but no HTTP endpoint creates or reads it directly in the current codebase — it appears intended for future CARE data sync. Include the table in the schema and `daily_rounds.rs` DB module, expose no API for it yet.

**Celery beat schedule**: The original runs `celery -A core.celery worker -B` which embeds the scheduler. The configured beat schedule is not in the repo (defaults to empty). Interval defaults are: automated observations = 60 min, camera sweep = configure same, S3 dump = 30 min. All three as `tokio::spawn` loops with `tokio::time::interval`.

**`serde` alias for `preset_name`**: `CameraAssetPresetRequest` uses `AliasChoices("preset", "presetName")`. In Rust, use `#[serde(alias = "presetName")]` on the `preset_name` field.

**`HOST_NAME` in S3 keys**: Used as prefix in `"{HOST_NAME}/{datetime}.json"`. Replicate exactly.

**`APP_VERSION` / `CARGO_PKG_VERSION`**: Use `env!("CARGO_PKG_VERSION")` for the version string, overridable by `APP_VERSION` env var.

**Sentry**: Add `sentry` crate integration. Initialize in `main.rs` if `SENTRY_DSN` is set. Capture panics and `AppError::Internal` variants. Use `sentry::integrations::tracing` layer.

**OpenAPI docs**: Add `utoipa` + `utoipa-swagger-ui` to expose `GET /api/docs/` and `GET /api/schema/`. Annotate all handlers. Equivalent to `drf-spectacular` in the original.

---

## Logging & Observability

The gateway uses the `tracing` crate for structured logging with comprehensive instrumentation across all subsystems. All requests from CARE and operations are logged to stdout for debugging and monitoring.

### Configuration

Logging is initialized at startup in `main.rs`:

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,teleicu_gateway=debug,tower_http=debug,axum=debug".into()),
    )
    .with_target(true)
    .with_thread_ids(true)
    .with_line_number(true)
    .init();
```

**Default levels:**
- `info` — global default for all crates
- `debug` — `teleicu_gateway` (our code), `tower_http` (HTTP middleware), `axum` (web framework)

**Customization via `RUST_LOG` environment variable:**
```sh
# Very verbose (includes trace-level ONVIF SOAP)
RUST_LOG=trace ./teleicu-gateway

# Only errors and warnings
RUST_LOG=warn,teleicu_gateway=info ./teleicu-gateway

# Debug specific modules
RUST_LOG=info,teleicu_gateway::camera=trace,teleicu_gateway::stream=debug ./teleicu-gateway
```

### HTTP Request/Response Logging

Tower's `TraceLayer` logs every HTTP request and response:

```rust
TraceLayer::new_for_http()
    .make_span_with(
        DefaultMakeSpan::new()
            .level(Level::INFO)
            .include_headers(true)
    )
    .on_request(DefaultOnRequest::new().level(Level::INFO))
    .on_response(
        DefaultOnResponse::new()
            .level(Level::INFO)
            .include_headers(true)
    )
```

**Example output:**
```
INFO tower_http::trace::on_request: started processing request method=POST uri=/getToken/videoFeed
INFO tower_http::trace::on_response: finished processing request latency=45ms status=200
```

### Subsystem-Specific Logging

All log messages use `target:` to identify their source module for easy filtering.

#### 1. Proxy to RTSPtoWeb (`teleicu_gateway::proxy`)

Every proxied request logs:
- Request method, path, and full URL to RTSPtoWeb
- Request/response headers (at `debug` level)
- Request/response body sizes
- Upstream response status
- Errors with full context

**Example:**
```
INFO  teleicu_gateway::proxy: 🔄 Proxying GET /stream to RTSPtoWeb: http://localhost:8080/stream?uuid=81087826-1e10-4a50-a4ec-ab0064d34745
DEBUG teleicu_gateway::proxy:   Request header: authorization: Bearer eyJ...
INFO  teleicu_gateway::proxy: ✅ RTSPtoWeb responded to /stream - Status: 200 OK
```

#### 2. Stream Token API (`teleicu_gateway::stream`)

Logs token generation and verification:
- `/getToken/videoFeed` — video stream token requests
- `/getToken/vitals` — vitals stream token requests
- `/verifyToken` — token validation from RTSPtoWeb
- `/verify_token` — CARE token exchange

**Example:**
```
INFO  teleicu_gateway::stream: 📹 Video stream token requested - stream: 81087826-1e10-4a50-a4ec-ab0064d34745, ip: 192.168.68.65, duration: Some("10")
INFO  teleicu_gateway::stream: ✅ Video stream token issued - stream: 81087826-1e10-4a50-a4ec-ab0064d34745, expires in: 600s
```

```
DEBUG teleicu_gateway::stream: 🔐 Token verification requested - ip: Some("192.168.68.65"), stream: Some("81087826...")
INFO  teleicu_gateway::stream: ✅ Token verified successfully - ip: Some("192.168.68.65"), stream: Some("81087826...")
```

**Errors:**
```
WARN  teleicu_gateway::stream: ❌ Token verification failed - invalid signature: InvalidSignature
WARN  teleicu_gateway::stream: ❌ Token verification failed - claims mismatch. Expected ip: Some("192.168.68.66"), stream: Some("uuid2"). Token claims: {"ip": "192.168.68.65", "stream": "uuid1"}
```

#### 3. Camera Control (`teleicu_gateway::camera`)

Logs all ONVIF operations:
- PTZ moves (absolute, relative, preset)
- Camera lock acquisition/release
- Preset management
- Status queries

**Example:**
```
INFO  teleicu_gateway::camera: 📷 POST /cameras/absoluteMove - hostname: 192.168.68.65, x: 0.5, y: -0.3, zoom: 0.0
DEBUG teleicu_gateway::camera: Acquiring camera lock for 192.168.68.65
DEBUG teleicu_gateway::camera: Camera lock acquired for 192.168.68.65
DEBUG teleicu_gateway::camera: Waiting for camera 192.168.68.65 to reach idle state
INFO  teleicu_gateway::camera: ✅ Absolute move completed for 192.168.68.65 to position (0.5, -0.3, 0.0)
```

```
INFO  teleicu_gateway::camera: 📷 POST /cameras/gotoPreset - hostname: 192.168.68.66, preset_index: 2
DEBUG teleicu_gateway::camera: Moving camera 192.168.68.66 to preset: Ventilator View (token: preset_2)
INFO  teleicu_gateway::camera: ✅ Camera 192.168.68.66 moved to preset #2 (Ventilator View)
```

#### 4. Authentication (`teleicu_gateway::auth`)

Logs CARE token validation:
- Authorization header extraction
- JWKS cache hits/misses
- Token validation attempts
- Key rotation tracking

**Example:**
```
DEBUG teleicu_gateway::auth: 🔐 Validating Care_Bearer token for POST /getToken/videoFeed
DEBUG teleicu_gateway::auth: Extracted Care_Bearer token (length: 324)
DEBUG teleicu_gateway::auth: Fetching CARE JWKS from cache or API
DEBUG teleicu_gateway::auth: ✅ Using cached JWKS (age: 142s, ttl: 300s)
DEBUG teleicu_gateway::auth: Retrieved JWKS with 2 keys
TRACE teleicu_gateway::auth: Trying JWKS key #0 (kid: Some("key-2024-01"))
INFO  teleicu_gateway::auth: ✅ Care_Bearer token validated successfully - sub: Some("user-123")
```

**JWKS fetch:**
```
DEBUG teleicu_gateway::auth: JWKS cache expired (age: 305s, ttl: 300s) - fetching fresh
INFO  teleicu_gateway::auth: 📡 Fetching JWKS from CARE API: https://care.example.com/api/gateway_device/jwks.json/
INFO  teleicu_gateway::auth: ✅ JWKS fetched successfully - 2 keys
DEBUG teleicu_gateway::auth: JWKS cache updated
```

**Errors:**
```
WARN  teleicu_gateway::auth: ❌ Missing Authorization header
WARN  teleicu_gateway::auth: ❌ Authorization header does not start with 'Care_Bearer '
WARN  teleicu_gateway::auth: ❌ Token validation failed - no valid key found in JWKS
ERROR teleicu_gateway::auth: ❌ Failed to fetch JWKS from https://care.example.com/api/gateway_device/jwks.json/: connection refused
```

### Emoji Guide

For quick visual scanning of logs:
- 🔄 — Proxying to RTSPtoWeb
- 📹 — Video stream token
- 💓 — Vitals stream token
- 🔐 — Authentication/token verification
- 📷 — Camera control operation
- 📡 — Network request (outbound to CARE)
- ✅ — Success
- ❌ — Error/failure
- 🚀 — Server startup
- 🔧 — Configuration/setup

### Debugging Camera Feed Issues

When camera feeds aren't working, check logs in this order:

**1. Server startup:**
```
INFO  Starting TeleICU Gateway v0.1.0 on 0.0.0.0:8090
INFO    RTSPTOWEB_URL  = http://localhost:8080
INFO  🔧 Building application router with proxy to http://localhost:8080
INFO  🚀 Server starting - listening on 0.0.0.0:8090
INFO  📡 Ready to accept requests from CARE and devices
```

**2. Stream token request from CARE:**
```
INFO  tower_http::trace::on_request: started processing request method=POST uri=/getToken/videoFeed
DEBUG teleicu_gateway::auth: 🔐 Validating Care_Bearer token for POST /getToken/videoFeed
INFO  teleicu_gateway::auth: ✅ Care_Bearer token validated successfully
INFO  teleicu_gateway::stream: 📹 Video stream token requested - stream: 81087826..., ip: 192.168.68.65
INFO  teleicu_gateway::stream: ✅ Video stream token issued - stream: 81087826..., expires in: 600s
INFO  tower_http::trace::on_response: finished processing request latency=23ms status=200
```

**3. Stream request via proxy:**
```
INFO  teleicu_gateway::proxy: 🔄 Proxying GET /stream to RTSPtoWeb: http://localhost:8080/stream?uuid=81087826-1e10-4a50-a4ec-ab0064d34745&token=eyJ...
INFO  teleicu_gateway::proxy: ✅ RTSPtoWeb responded to /stream - Status: 200 OK
```

**4. Token verification callback from RTSPtoWeb:**
```
INFO  tower_http::trace::on_request: started processing request method=POST uri=/verifyToken
DEBUG teleicu_gateway::stream: 🔐 Token verification requested - stream: Some("81087826...")
INFO  teleicu_gateway::stream: ✅ Token verified successfully
```

**Common failure patterns:**

- **No proxy logs** → CARE never requested stream, or routing issue
- **401 on /getToken/videoFeed** → CARE token invalid, check JWKS
- **Proxy connection refused** → RTSPtoWeb not running on expected port
- **No /verifyToken callback** → RTSPtoWeb not configured with `token.backend`
- **Token verification fails** → UUID mismatch or token expired

### Log Levels Summary

| Level | What's Logged |
|---|---|
| `ERROR` | Fatal errors, CARE API failures, proxy failures |
| `WARN` | Authentication failures, invalid tokens, deprecated usage |
| `INFO` | All requests, responses, token generation, camera operations |
| `DEBUG` | Headers, cache hits/misses, lock acquisition, intermediate steps |
| `TRACE` | JWKS key attempts, SOAP envelopes (very verbose) |

## Startup Sequence (`main.rs`)

The binary is zero-config-friendly — it creates missing directories and the SQLite file on first run. The boot sequence is:

1. Load `.env` via `dotenvy` (logs whether a file was found and from where).
2. Init `tracing-subscriber` with `RUST_LOG` env filter (default `info,teleicu_gateway=debug`).
3. Load `Settings::from_env()`.
4. Log a full config summary: cwd, `DATABASE_URL`, `STATE_DIR`, `CARE_API`, `RTSPTOWEB_URL`, `GATEWAY_DEVICE_ID` presence, S3/Sentry/JWKS status, automated-obs settings.
5. **Pre-flight checks**: ensure the database parent directory and `STATE_DIR` exist, creating them with `create_dir_all` if needed. Errors here include the resolved path and cwd for debuggability.
6. Init Sentry (if `SENTRY_DSN` set).
7. Connect to SQLite via `SqlitePoolOptions`. The default URL uses `?mode=rwc` so the file is created if absent. The error message on failure includes the URL and cwd.
8. Run `sqlx::migrate!("./migrations")`.
9. `OwnKeypair::load_or_generate` — tries `JWKS_BASE64` env → `{STATE_DIR}/jwks.json` file → generate new RSA-2048, persists to file.
10. Build `AppState`, spawn background tasks, bind and serve.

When debugging startup failures: check the cwd (the binary logs it), whether `DATABASE_URL` includes `?mode=rwc`, and whether `STATE_DIR` is writable.

---

## Build & Run

**Cargo** (requires SQLite and, on macOS, libiconv via Nix dev shell or Xcode CLI tools):
```
cargo build --release          # binary at target/release/teleicu-gateway
```

**Nix** (hermetic, handles all native deps):
```
nix develop                    # enter dev shell with all deps
nix build                      # production binary at result/bin/teleicu-gateway
```

On macOS, always use `nix develop` for building — the project depends on `libiconv` and `sqlite` which the flake's dev shell provides. Running `cargo build` outside the shell will fail with linker errors.

**Running**: copy `.env.example` to `.env`, fill in `GATEWAY_DEVICE_ID`, and run the binary. Everything else has working defaults.

---

## Axum Extractor Caveat

`axum-core` 0.4.x defines `FromRequestParts` with the `#[async_trait]` macro (from the `async-trait` crate), not native Rust async-in-trait. Any `impl FromRequestParts` **must** also carry the `#[async_trait]` attribute and import `use async_trait::async_trait`. Writing a bare `async fn from_request_parts` without the attribute produces a confusing `E0195` lifetime mismatch error. This applies to `CareAuth` in `src/auth/inbound.rs` and any future custom extractors.

---

# Plans

## Plan 1: Testing Framework — Unit Tests + End-to-End Tests

### Goal

Make the binary bulletproof by building a comprehensive test suite that covers every module's happy path, error paths, and edge cases. Tests must be runnable with `cargo test` (no external services required for unit tests) and with a single command for E2E tests.

### Dev Dependencies to Add (`Cargo.toml [dev-dependencies]`)

```toml
[dev-dependencies]
tokio-test      = "0.4"
axum-test       = "15"          # or tower::ServiceExt-based testing
tempfile        = "3"           # temp dirs for SQLite, STATE_DIR, keypair files
wiremock        = "0.6"         # mock HTTP server for CARE API + ONVIF cameras
pretty_assertions = "1"         # better diff output on assertion failures
```

No test binary or custom harness needed — standard `#[cfg(test)] mod tests` in each file plus `tests/` directory for integration/E2E tests.

### Test Helper Module (`src/test_helpers.rs` — `#[cfg(test)]` gated)

A shared test utility module providing:

- **`test_settings()`** — returns a `Settings` with safe defaults: bind to `127.0.0.1:0`, `database_url = "sqlite::memory:"`, empty `gateway_device_id`, S3 disabled, Sentry disabled, `state_dir` pointing to a `tempfile::TempDir`, random `encryption_key`.
- **`test_app_state()`** — builds a full `AppState` backed by an in-memory SQLite DB with migrations applied, a freshly-generated `OwnKeypair`, a real `reqwest::Client`, and empty `ObservationStore` / `CameraLockMap`. Returns `(AppState, TempDir)` so the tempdir lives long enough.
- **`test_router(state: AppState)`** — returns the full `Router<AppState>` with all routes wired (extracted from `main.rs` into a standalone `fn build_router(state, rtsptoweb_url) -> Router` to make it testable).
- **`make_observation(device_id, obs_id, value)`** — factory for `Observation` structs with sensible defaults for all 20+ fields.
- **`make_bp_observation(device_id, systolic, diastolic, map)`** — factory for blood-pressure observations.
- **`make_waveform_observation(device_id, wave_name)`** — factory for waveform observations.
- **`sign_care_jwt(keypair, extra_claims, exp_secs)`** — shorthand for signing a test JWT.

### Refactor: Extract Router Builder

Move the `Router::new().route(...)...` block in `main.rs` into:

```rust
// src/router.rs
pub fn build_router(state: AppState, rtsptoweb_url: String) -> Router { ... }
```

`main.rs` calls `build_router(state, settings.rtsptoweb_url.clone())` — no behaviour change, but E2E tests can now spin up an identical router without duplicating route definitions.

---

### Layer 1: Unit Tests (inline `#[cfg(test)]` modules)

#### `src/observations/validity.rs`

| Test | Description |
|---|---|
| `test_valid_heart_rate` | HR observation with value=72.0 and status="final" → `is_valid` returns true |
| `test_zero_value_is_invalid` | HR with value=0.0 → false |
| `test_none_value_is_invalid` | HR with value=None → false |
| `test_negative_value_is_invalid` | SpO2 with value=-1.0 → false |
| `test_sensor_off_status` | status="Message-Sensor Off" (case-insensitive) → false |
| `test_leads_off_status` | status="message-leads off" → false |
| `test_probe_off_status` | status="message-probe off" → false |
| `test_artifact_status` | status="message-artifact" → false |
| `test_disconnected_status` | status="disconnected" → false |
| `test_error_status` | status="error" → false |
| `test_all_invalid_status_strings` | Loop through all 15 `INVALID_STATUS_STRINGS`, verify each returns false |
| `test_valid_bp_with_systolic` | BP obs with systolic.value=Some(120.0) → true |
| `test_bp_missing_systolic` | BP obs with systolic=None → false |
| `test_bp_zero_systolic` | BP obs with systolic.value=Some(0.0) → false |
| `test_bp_no_systolic_value` | BP obs with systolic=Some but value=None → false |
| `test_waveform_always_valid` | Waveform, WaveformII, WaveformPleth, WaveformRespiration with any status → true |
| `test_device_connection_always_valid` | DeviceConnection with any status → true |
| `test_valid_status_with_good_value` | status="final" value=98.0 → true |
| `test_status_substring_match` | status="some message-leads off here" → false (substring match) |
| `test_body_temperature_valid` | BodyTemperature1 with value=36.5 → true |

#### `src/observations/store.rs`

| Test | Description |
|---|---|
| `test_new_store_is_empty` | Fresh store → `get_device_statuses()` is empty |
| `test_ingest_single_observation` | Ingest 1 obs → device status shows "up", buffer has 1 entry |
| `test_ingest_sets_taken_at` | Ingest obs → `taken_at` is set to approximately `Utc::now()` |
| `test_ingest_empty_vec` | Ingest empty vec → no-op, no crash |
| `test_ring_buffer_max_size` | Ingest >7200 obs for one device → buffer stays at 7200, oldest evicted |
| `test_multi_device_isolation` | Ingest obs for device A and B → buffers are independent |
| `test_bp_carry_forward` | Ingest [HR, BP], then ingest [HR] → second batch broadcast includes the carried-forward BP |
| `test_bp_carry_forward_replaces` | Ingest [BP(120)], then [BP(130)], then [HR] → carried-forward BP has systolic=130 |
| `test_bp_carry_forward_only_valid` | Ingest [BP with status="sensor off"], then [HR] → no carry-forward (invalid BP not stored) |
| `test_subscribe_receives_broadcast` | Subscribe to device A, then ingest → subscriber receives the batch |
| `test_subscribe_no_data_no_crash` | Subscribe then drop sender → receiver gets `Closed` error cleanly |
| `test_get_static_returns_latest_per_type` | Ingest HR=70, HR=80, SpO2=95 → `get_static` returns HR=80 and SpO2=95 |
| `test_get_static_excludes_waveforms` | Ingest HR + Waveform → `get_static` returns only HR |
| `test_get_static_respects_since_cutoff` | Ingest old obs (>interval), then recent → only recent returned |
| `test_get_static_no_data_returns_none` | `get_static("nonexistent")` → None |
| `test_drain_stale_removes_old` | Ingest obs, artificially set old `taken_at`, drain → old ones removed |
| `test_drain_stale_keeps_recent` | Drain with short duration → recent obs untouched |
| `test_set_device_status` | `set_device_status("cam1", "down")` → `get_device_statuses` has cam1=down |
| `test_device_status_updates_on_ingest` | Ingest for device → status is "up" with recent time |

#### `src/observations/types.rs`

| Test | Description |
|---|---|
| `test_observation_id_serde_roundtrip` | Serialize+deserialize each `ObservationId` variant → matches |
| `test_observation_id_display` | `HeartRate.to_string()` == "heart-rate", etc. |
| `test_observation_code_mapping` | All `AUTOMATED_OBSERVATION_TYPES` have a non-None `observation_code` |
| `test_unit_code_mapping` | All `AUTOMATED_OBSERVATION_TYPES` have a non-None `unit_code` |
| `test_waveform_has_no_observation_code` | `observation_code(Waveform)` → None |
| `test_observation_json_structure` | Deserialize a full sample JSON → all fields map correctly (especially renamed fields like `date-time`, `patient-id`, `low-limit`) |
| `test_blood_pressure_nested_deser` | Deserialize a BP observation with systolic/diastolic/map nested objects |

#### `src/onvif/soap.rs`

| Test | Description |
|---|---|
| `test_ws_security_header_structure` | Parse output XML → has `wsse:UsernameToken`, `wsse:Username`, `wsse:Password`, `wsse:Nonce`, `wsu:Created` elements |
| `test_ws_security_digest_correctness` | Known nonce + created + password → known digest (pin one test vector by fixing the nonce) |
| `test_soap_envelope_structure` | Output contains `<s:Envelope>`, `<s:Header>`, `<s:Body>`, the WS-Security block, and the provided body |
| `test_get_profiles_body` | Contains `<GetProfiles xmlns="...media..."/>` |
| `test_get_status_body` | Contains `<ProfileToken>tok</ProfileToken>` |
| `test_get_presets_body` | Contains correct `<GetPresets>` XML |
| `test_goto_preset_body` | Contains both `<ProfileToken>` and `<PresetToken>` |
| `test_set_preset_body` | Contains `<PresetName>` |
| `test_absolute_move_body` | Contains `<Position>` with `<PanTilt x="..." y="...">` and `<Zoom x="...">` |
| `test_relative_move_body` | Contains `<Translation>` (not `<Position>`) |
| `test_get_snapshot_uri_body` | Contains `<GetSnapshotUri>` |
| `test_float_formatting` | pan=0.5, tilt=-0.3, zoom=0.0 → correctly embedded as strings in XML |

#### `src/onvif/client.rs` — XML parser tests

| Test | Description |
|---|---|
| `test_parse_profiles_real_xml` | Feed a realistic ONVIF GetProfiles response → correct `Vec<Profile>` |
| `test_parse_profiles_empty` | Empty SOAP body → empty vec |
| `test_parse_profiles_multiple` | Two `<Profiles>` elements → two entries |
| `test_parse_ptz_status_idle` | XML with MoveStatus IDLE → `PtzStatus.move_status.pan_tilt == "IDLE"` |
| `test_parse_ptz_status_moving` | XML with MoveStatus MOVING → correctly detected |
| `test_parse_ptz_status_noerror` | Error field = "NO error" → normalised to `None` |
| `test_parse_ptz_status_with_error` | Error field = "CommunicationError" → `Some("CommunicationError")` |
| `test_parse_presets_real_xml` | Realistic presets response → correct `Vec<Preset>` |
| `test_parse_presets_nameless` | Preset without `<Name>` element → token present, name is empty string |
| `test_parse_snapshot_uri` | Response with `<Uri>http://...</Uri>` → extracted correctly |
| `test_parse_snapshot_uri_missing` | No `<Uri>` element → returns `AppError::Onvif` |

#### `src/onvif/lock.rs`

| Test | Description |
|---|---|
| `test_lock_acquire_succeeds` | Fresh map → `try_lock("cam1")` succeeds |
| `test_lock_concurrent_blocks` | Hold lock on "cam1", second `try_lock("cam1")` with short timeout → `CameraLocked` error |
| `test_lock_release_on_drop` | Acquire lock, drop guard, re-acquire → succeeds |
| `test_different_ips_independent` | Lock "cam1" → lock "cam2" succeeds concurrently |
| `test_lock_timeout_configurable` | Timeout=1s, hold lock for 2s → second attempt returns `CameraLocked` |

#### `src/config.rs`

| Test | Description |
|---|---|
| `test_defaults` | No env vars set (clear all) → `Settings::from_env()` uses all defaults: host=0.0.0.0, port=8090, etc. |
| `test_s3_configured_all_set` | Set all 3 S3 vars → `s3_configured()` true |
| `test_s3_configured_partial` | Only access_key set → `s3_configured()` false |
| `test_automated_obs_enabled_from_device_id` | `GATEWAY_DEVICE_ID` set, no explicit enabled → enabled=true |
| `test_automated_obs_disabled_explicit` | `AUTOMATED_OBSERVATIONS_ENABLED=false` → enabled=false even with device_id set |
| `test_empty_optional_strings_become_none` | Set `SENTRY_DSN=""` → `sentry_dsn` is None |
| `test_bind_port_parse_error` | `BIND_PORT=abc` → `from_env()` returns Err |

#### `src/error.rs`

| Test | Description |
|---|---|
| `test_not_found_response` | `AppError::NotFound.into_response()` → 404 with `{"error":"not found"}` |
| `test_unauthorized_response` | → 401 |
| `test_camera_locked_response` | → 409 |
| `test_invalid_camera_credentials_response` | → 400 |
| `test_onvif_error_response` | → 502 |
| `test_care_api_error_response` | → 502 |
| `test_db_error_response` | → 500, body says "database error" (not leaking details) |
| `test_internal_error_response` | → 500, body says "internal error" (not leaking details) |

#### `src/auth/outbound.rs`

| Test | Description |
|---|---|
| `test_generate_keypair` | `load_or_generate(tempdir, None)` with no existing file → generates keypair, file created |
| `test_load_from_file` | Generate, then load again → same public key `n` and `e` |
| `test_load_from_base64` | Base64-encode a generated JWKS → `load_or_generate(dir, Some(b64))` succeeds |
| `test_sign_and_verify_roundtrip` | `sign_jwt(claims, 300)` → `verify_jwt(token)` → claims match |
| `test_verify_expired_token` | `sign_jwt(claims, 0)` (immediate expiry) → `verify_jwt` fails |
| `test_verify_garbage_token` | `verify_jwt("not.a.jwt")` → error |
| `test_public_jwks_structure` | `public_jwks()` → has `keys` array with one entry, fields `kty`, `use`, `alg`, `n`, `e` |

#### `src/db/assets.rs`

| Test | Description |
|---|---|
| `test_encrypt_decrypt_roundtrip` | Encrypt "mypassword" → decrypt → "mypassword" |
| `test_decrypt_wrong_key` | Encrypt with key A, decrypt with key B → error |
| `test_decrypt_too_short` | Decrypt 5 bytes → error |
| `test_encrypt_bad_key_length` | Key of 16 bytes (not 32) → error |
| `test_hex_decode_valid` | "48656c6c6f" → [72,101,108,108,111] |
| `test_hex_decode_odd_length` | "123" → error |
| `test_create_and_get_asset` | Create asset → get by ID → fields match |
| `test_list_assets_empty` | Fresh DB → empty vec |
| `test_list_assets_by_type` | Create ONVIF + HL7MONITOR → list(type=ONVIF) returns only ONVIF |
| `test_soft_delete` | Create → delete → list returns empty, get still returns (with deleted=true) |
| `test_delete_nonexistent` | Delete random UUID → `NotFound` |
| `test_asset_type_fromstr` | "ONVIF" → Ok, "INVALID" → Err |

#### `src/api/stream.rs`

| Test | Description |
|---|---|
| `test_parse_duration_none` | None → 5 |
| `test_parse_duration_valid` | Some("10") → 10 |
| `test_parse_duration_clamp_high` | Some("120") → 60 |
| `test_parse_duration_clamp_low` | Some("0") → 1 |
| `test_parse_duration_non_numeric` | Some("abc") → 5 |

#### `src/tasks/automated_observations.rs`

| Test | Description |
|---|---|
| `test_build_observation_specs_heart_rate` | HR obs value=72 → spec with LOINC code 8867-4, unit /min, value "72" |
| `test_build_observation_specs_spo2` | SpO2 obs value=98 → LOINC 2708-6, unit % |
| `test_build_observation_specs_bp` | BP obs systolic=120 → LOINC 85354-9, unit mmHg, value "120" |
| `test_build_observation_specs_skips_waveform` | Waveform obs → empty specs |
| `test_build_observation_specs_skips_invalid` | HR with status="sensor off" → empty specs |
| `test_build_observation_specs_with_reference_range` | HR with low_limit=60 high_limit=100 → reference_range populated |
| `test_build_observation_specs_integer_vs_decimal` | value=72.0 → value_type="integer"; value=36.5 → value_type="decimal" |

---

### Layer 2: Integration / E2E Tests (`tests/` directory)

These tests spin up a real Axum server (in-memory SQLite, mock CARE API via `wiremock`) and make actual HTTP requests.

#### Test Harness (`tests/common/mod.rs`)

- `spawn_app()` → boots the full app on a random port with an in-memory DB, returns `TestApp { addr, state, care_mock: MockServer }`.
- `TestApp::url(path)` → `format!("http://{}{}", self.addr, path)`.
- `TestApp::care_bearer_header()` → signs a JWT with the app's own keypair (for routes that require `CareAuth`, the test configures the CARE JWKS mock to return the app's own public key, so the same key works for both directions).

#### `tests/health_endpoints.rs`

| Test | Description |
|---|---|
| `test_healthz` | GET `/healthz` → 200, body has `server: "ok"`, `database: "ok"` |
| `test_ping` | GET `/health/ping` → 200, body has `pong` with an RFC3339 timestamp |
| `test_status` | GET `/health/status` → 200, body has `version` field |
| `test_care_communication_success` | Mock CARE `/middleware/verify` → 200 → endpoint returns the mocked body |
| `test_care_communication_unreachable` | No mock → endpoint returns error gracefully |

#### `tests/observation_endpoints.rs`

| Test | Description |
|---|---|
| `test_update_observations_from_loopback` | POST `/update_observations` from 127.0.0.1 with valid JSON → 200 |
| `test_update_observations_empty_array` | POST `[]` → 200 (no-op) |
| `test_update_observations_malformed_json` | POST garbage → 4xx |
| `test_device_status_requires_auth` | GET `/devices/status` with no Authorization header → 401 |
| `test_device_status_with_auth` | GET `/devices/status` with valid Care_Bearer → 200 with device status map |
| `test_device_status_after_ingest` | Ingest obs for device A → GET `/devices/status` → device A shows "up" |

#### `tests/camera_endpoints.rs`

| Test | Description |
|---|---|
| `test_get_presets_requires_auth` | No auth → 401 |
| `test_get_camera_status_requires_auth` | No auth → 401 |
| `test_absolute_move_requires_auth` | No auth → 401 |
| `test_goto_preset_missing_preset_index` | Valid auth, body without `preset` → appropriate error |
| `test_camera_endpoints_bad_credentials` | Point at wiremock returning 401 → `InvalidCameraCredentials` |

#### `tests/stream_endpoints.rs`

| Test | Description |
|---|---|
| `test_get_video_feed_token` | POST `/getToken/videoFeed` with auth → 200, body has `token` field |
| `test_get_vitals_token` | POST `/getToken/vitals` with auth → 200, body has `token` |
| `test_verify_token_valid` | Generate token → POST `/verifyToken` with matching ip → status "1" |
| `test_verify_token_invalid` | POST `/verifyToken` with garbage token → status "0", 401 |
| `test_verify_token_wrong_ip` | Generate token for ip=A → verify with ip=B and no stream → status "0" |
| `test_exchange_token_care_rejects` | Mock CARE verify → 401 → our endpoint returns Unauthorized |
| `test_exchange_token_care_accepts` | Mock CARE verify → 200 → our endpoint returns a gateway JWT |

#### `tests/openid_configuration.rs`

| Test | Description |
|---|---|
| `test_openid_configuration_returns_jwks` | GET `/openid-configuration/` → 200, body has `keys` array with at least one RSA key |
| `test_openid_key_has_required_fields` | Key has `kty`, `use`, `alg`, `n`, `e` |

#### `tests/websocket.rs`

| Test | Description |
|---|---|
| `test_ws_observations_no_token` | Connect to `/observations/1.2.3.4` without Sec-WebSocket-Protocol → immediately closed |
| `test_ws_observations_bad_token` | Connect with invalid JWT → immediately closed |
| `test_ws_observations_receives_data` | Connect with valid token → ingest observations for that IP → WS receives the JSON batch |
| `test_ws_logger_streams_metrics` | Connect to `/logger` → receive at least one message with `type: "RESOURCE"`, `cpu`, `memory`, `uptime`, `load` |

#### `tests/auth_flow.rs`

| Test | Description |
|---|---|
| `test_care_bearer_valid_token` | Sign JWT with the mock CARE private key → `CareAuth` extractor succeeds |
| `test_care_bearer_expired_token` | Sign JWT with exp in the past → 401 |
| `test_care_bearer_wrong_key` | Sign JWT with a different RSA key → 401 |
| `test_care_bearer_missing_header` | No Authorization header → 401 |
| `test_care_bearer_wrong_prefix` | `Authorization: Bearer <token>` (not `Care_Bearer`) → 401 |
| `test_jwks_cache_refresh` | First request fetches JWKS, second within 5min uses cache (verify with mock hit count) |

#### `tests/proxy.rs`

| Test | Description |
|---|---|
| `test_proxy_start_route` | Mock rtsptoweb → GET `/start` → proxied correctly, response matches |
| `test_proxy_preserves_query_params` | GET `/stream?id=foo` → upstream sees `?id=foo` |
| `test_proxy_forwards_post_body` | POST `/stop` with body → upstream receives same body |
| `test_proxy_upstream_down` | No mock → returns 5xx gracefully |

---

### Layer 3: Edge-Case & Stress Tests

| Test | Description |
|---|---|
| `test_concurrent_observation_ingest` | 100 tokio tasks ingesting simultaneously → no panics, no data corruption |
| `test_concurrent_camera_lock_contention` | 10 tasks trying to lock same camera → exactly 1 succeeds, rest get `CameraLocked` |
| `test_observation_store_memory_bound` | Ingest 100k observations → buffer per device stays at 7200 |
| `test_drain_stale_under_concurrent_ingest` | Drain + ingest simultaneously → no deadlock |
| `test_ws_client_disconnect_during_broadcast` | Subscribe, ingest, drop WS mid-send → server doesn't panic |
| `test_large_observation_batch` | Ingest 10,000 observations in one POST → succeeds |
| `test_malformed_observation_fields` | Missing required fields → serde returns 4xx, not 500 |
| `test_unicode_in_patient_name` | patient_name with CJK / emoji characters → roundtrips correctly |
| `test_very_long_device_id` | 10KB device_id string → doesn't crash (may reject) |
| `test_s3_dump_no_stale_data` | S3 dump with no stale data → no-op, no S3 call |
| `test_keypair_file_permissions` | Generated jwks.json is not world-readable (Unix only) |

---

### CI Configuration

Add a GitHub Actions workflow (`.github/workflows/test.yml`):

1. **`cargo fmt --check`** — enforce formatting.
2. **`cargo clippy -- -D warnings`** — zero warnings policy.
3. **`cargo test`** — run all unit + integration tests.
4. **`cargo test -- --ignored`** — optional slow/stress tests gated behind `#[ignore]`.

All tests should run in under 60 seconds on CI. Use `#[ignore]` for stress tests that take longer.

---

### Implementation Order

1. Add dev-dependencies to `Cargo.toml`.
2. Refactor: extract `build_router()` into `src/router.rs`.
3. Create `src/test_helpers.rs` with `test_settings()`, `test_app_state()`, factory functions.
4. Write unit tests for pure-logic modules first (fastest feedback loop):
   - `observations/validity.rs`
   - `observations/types.rs`
   - `onvif/soap.rs`
   - `onvif/client.rs` (XML parsers)
   - `onvif/lock.rs`
   - `error.rs`
   - `config.rs`
   - `db/assets.rs` (encryption + DB CRUD)
   - `api/stream.rs` (`parse_duration_mins`)
   - `auth/outbound.rs` (keypair + JWT sign/verify)
   - `observations/store.rs`
   - `tasks/automated_observations.rs` (`build_observation_specs`)
5. Write E2E tests using `wiremock` + real Axum server:
   - `tests/common/mod.rs` (harness)
   - Health, observation, camera, stream, openid, websocket, auth, proxy test files.
6. Write stress/edge-case tests (tagged `#[ignore]`).
7. Add CI workflow.

---

## Plan 2: Status Page at `GET /`

### Goal

Serve an HTML status dashboard at the root path (`/`) of the web server so that operators can open the gateway's address in a browser and immediately see whether everything is healthy — database connectivity, CARE API reachability, connected devices, background task health, and system resource usage.

### Route

Add `GET /` → `api::status::index` returning `Content-Type: text/html`.

### Implementation

#### New File: `src/api/status.rs`

Handler: `pub async fn index(State(state): State<AppState>) -> impl IntoResponse`

The handler gathers all status data and renders an HTML page (no template engine — just a raw string with inline CSS, keeping dependencies at zero). The page auto-refreshes every 30 seconds via `<meta http-equiv="refresh" content="30">`.

#### Data Gathered

| Section | Source | Details |
|---|---|---|
| **Gateway Info** | `state.settings` | App version, hostname, gateway device ID, bind address, uptime (track start time in `AppState`) |
| **Database** | `sqlx::query("SELECT 1")` on `state.db` | Status: ✅ OK or ❌ Error |
| **CARE API** | `state.http.get(care_api + "/middleware/verify")` | Status: ✅ Reachable / ❌ Unreachable, response time in ms |
| **RSA Keypair** | `state.own_keypair` | Key ID (first 8 chars of base64url `n`), algorithm (RS256) |
| **S3** | `state.settings.s3_configured()` | Configured: Yes/No, bucket name |
| **Sentry** | `state.settings.sentry_dsn` | Configured: Yes/No |
| **Connected Devices** | `state.obs_store.get_device_statuses()` | Table: Device IP, Status (up/down with colored badge), Last Seen timestamp |
| **Observation Store** | `state.obs_store` | Total devices tracked, total observations buffered (sum of all ring buffer lengths) |
| **Background Tasks** | New `TaskHealth` struct in `AppState` | Last run time + status for: automated observations, camera status sweep, S3 dump |
| **System Resources** | `sysinfo::System` | CPU %, memory %, uptime — same data as the `/logger` WebSocket but as a one-shot snapshot |

#### Add to `AppState`

```rust
pub struct TaskHealth {
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: String,  // "ok", "error: <msg>", "disabled"
}

// In AppState:
pub started_at: DateTime<Utc>,
pub task_health: Arc<DashMap<String, TaskHealth>>,
```

Background tasks update `task_health` after each cycle (key = "automated_observations" / "camera_status" / "s3_dump").

#### Add Observation Store Method

Add `pub fn total_buffered(&self) -> usize` to `ObservationStore` — sums all ring buffer lengths for the status page.

#### HTML Design

Minimal, dependency-free HTML with inline CSS. Dark-themed, responsive, single-page. Sections:

1. **Header** — "TeleICU Gateway" + version badge + hostname.
2. **Health Summary** — 3 status cards: Database, CARE API, S3. Green/red badges.
3. **Devices Table** — sortable by status, shows IP, status badge, last seen (relative time like "2m ago").
4. **Background Tasks Table** — task name, last run, status, next run (estimated from interval).
5. **System Resources** — CPU bar, memory bar, uptime.
6. **Footer** — auto-refresh countdown, link to `/healthz` JSON endpoint.

#### Route Registration in `main.rs`

```rust
.route("/", get(api::status::index))
```

Add `pub mod status;` to `src/api/mod.rs`.

### Implementation Order

1. Add `started_at: DateTime<Utc>` and `task_health: Arc<DashMap<String, TaskHealth>>` to `AppState`.
2. Update background tasks to write to `task_health` after each cycle.
3. Add `ObservationStore::total_buffered()`.
4. Create `src/api/status.rs` with the HTML rendering handler.
5. Register `GET /` route in the router.
6. Add tests: `test_status_page_returns_html` (200, content-type text/html), `test_status_page_contains_version`.

## Plan 3: Binary Generation with Spindle on Tangled.org

### Goal

Produce release binaries for Linux using the Spindle workflow runner on Tangled.org. Each push to `main` (or tagged release) triggers Spindle to build an optimized binary and run CI checks. Spindle uses the **nixery** engine, which constructs container images on-the-fly from **nixpkgs** — no Dockerfile needed.

### Key Learnings from Tangled/Spindle Docs

**Spindle is NOT GitHub Actions.** The workflow format is entirely different:

- **Directory**: `.tangled/workflows/` (not `.github/workflows/` or `.spindle/`)
- **Trigger**: `when:` field with `event:` and `branch:`/`tag:` (not `on:`)
- **Engine**: `engine: nixery` (not `runs-on:`)
- **Dependencies**: `dependencies.nixpkgs:` lists nix packages (not `apt-get` or `uses:`)
- **Steps**: Flat list of `name:` + `command:` pairs (not `jobs:` with nested `steps:`)
- **No matrix builds**: Each workflow is a single sequential pipeline
- **No `uses:` actions**: No `actions/checkout`, `actions/upload-artifact`, etc. — the repo is auto-cloned
- **No `runs-on:`**: The engine field determines the execution environment
- **Per-step `environment:`**: Environment variables can be set globally or per-step

Reference: Tangled core repo `.tangled/workflows/` at https://tangled.org/tangled.org/core

### Supported Targets (Linux Only)

| Target Triple | Notes |
|---|---|
| `x86_64-unknown-linux-gnu` | Primary deployment target (NixOS, Debian, Ubuntu). Native build on nixery x86_64 runner. |

> **Future targets** (require cross-compilation tooling or separate runners, deferred for now):
> - `x86_64-unknown-linux-musl` — Static binary, needs musl toolchain in nix
> - `aarch64-unknown-linux-gnu` — ARM64, needs cross-linker
> - `aarch64-unknown-linux-musl` — Static ARM64, needs both

### Spindle CI Workflow (`.tangled/workflows/ci.yml`)

Runs on every push/PR to `main`. Checks formatting, lints, and runs tests.

```yaml
# .tangled/workflows/ci.yml
when:
  - event: ["push", "pull_request"]
    branch: main

engine: nixery

dependencies:
  nixpkgs:
    - cargo
    - rustc
    - rustfmt
    - clippy
    - gcc
    - pkg-config
    - sqlite
    - perl
    - binutils

steps:
  - name: Check formatting
    command: cargo fmt --check

  - name: Clippy lint
    command: cargo clippy -- -D warnings

  - name: Run tests
    command: cargo test
```

#### Nixpkgs dependency rationale

| Package | Why |
|---|---|
| `cargo`, `rustc` | Rust toolchain from nixpkgs stable |
| `rustfmt`, `clippy` | Separate nixpkgs packages for formatting and linting |
| `gcc` | C linker + compiler for `ring`, `libsqlite3-sys` build scripts |
| `pkg-config` | Build script dependency resolution (sqlite, etc.) |
| `sqlite` | System sqlite3 headers/lib for `sqlx` `sqlite` feature |
| `perl` | Required by `ring` crate's build script |
| `binutils` | `strip` command for reducing binary size |

### Spindle Build Workflow (`.tangled/workflows/build.yml`)

Runs on pushes to `main` and on version tags. Builds a release-optimized binary for x86_64 Linux.

```yaml
# .tangled/workflows/build.yml
when:
  - event: push
    branch: main
    tag: ["v*"]

engine: nixery

dependencies:
  nixpkgs:
    - cargo
    - rustc
    - gcc
    - pkg-config
    - sqlite
    - perl
    - binutils
    - coreutils

steps:
  - name: Build release binary
    command: cargo build --release

  - name: Strip binary
    command: strip target/release/teleicu-gateway

  - name: Show binary info
    command: |
      ls -lh target/release/teleicu-gateway
      file target/release/teleicu-gateway

  - name: Package artifact
    command: |
      mkdir -p dist
      cp target/release/teleicu-gateway dist/teleicu-gateway-x86_64-unknown-linux-gnu
      sha256sum dist/teleicu-gateway-x86_64-unknown-linux-gnu > dist/teleicu-gateway-x86_64-unknown-linux-gnu.sha256
      cat dist/teleicu-gateway-x86_64-unknown-linux-gnu.sha256
```

### How Spindle Works (Architecture Notes)

1. **Trigger**: Spindle listens to repo events via AT Protocol Jetstream. When a push matches a workflow's `when:` conditions, the pipeline is queued.
2. **Clone**: The repo is automatically cloned into `/tangled/workspace` (depth 1 by default). No `actions/checkout` needed.
3. **Container**: Nixery constructs a container image on-the-fly containing the listed nixpkgs packages. Layers are cached for frequently used packages.
4. **Steps**: Each step runs sequentially in the same container. State in `/tangled/workspace` persists across steps within a single pipeline run.
5. **Logs**: Step output is streamed via WebSocket and visible on the Tangled pipelines page.
6. **Timeout**: Default workflow timeout is 5 minutes (configurable by spindle operator via `SPINDLE_PIPELINES_WORKFLOW_TIMEOUT`).
7. **Secrets**: Can be added via the repository settings page on Tangled. They're injected as environment variables at runtime. **Never put secrets in the workflow YAML** — those are visible to anyone viewing the repo.
8. **Built-in env vars**: `CI=true`, `TANGLED_SHA`, `TANGLED_REF_NAME`, `TANGLED_REF_TYPE`, `TANGLED_REPO_NAME`, etc. are automatically available.

### Binary Naming Convention

Artifacts follow the pattern: `teleicu-gateway-{target_triple}`

Primary artifact:
- `teleicu-gateway-x86_64-unknown-linux-gnu`
- `teleicu-gateway-x86_64-unknown-linux-gnu.sha256`

### Versioning

The binary version is derived from `Cargo.toml` via `env!("CARGO_PKG_VERSION")` at compile time. Tags must match the `Cargo.toml` version (e.g., tag `v0.1.0` corresponds to `version = "0.1.0"` in `Cargo.toml`).

### Important: No TLS Feature Flags Needed

The project already uses `rustls` everywhere (no OpenSSL dependency):
- `reqwest` with `rustls-tls` and `default-features = false`
- `sqlx` with `runtime-tokio-rustls`
- `jsonwebtoken` uses `ring` (pure Rust/asm)
- `rsa` crate is pure Rust

This means we do **not** need `openssl` or `openssl.dev` in nixpkgs dependencies, and no feature flag changes are needed for the build.

### Implementation Order

1. Create `.tangled/workflows/ci.yml` with fmt, clippy, and test steps.
2. Create `.tangled/workflows/build.yml` with release build + strip + package steps.
3. Test locally with `cargo build --release` to confirm the build works.
4. Push to Tangled.org, verify Spindle picks up and runs both workflows.
5. Check pipeline logs on the Tangled pipelines page for the repo.
6. Tag a test release (`v0.0.1-rc1`), verify the build workflow triggers on the tag.
7. (Future) Add musl static build workflow once musl cross-compilation is validated in nixery.

---

## Plan 4: Drop-in Replacement for Django Middleware — Camera API Route Fixes

### Goal

Make the Rust gateway a **drop-in replacement** for the original Django middleware by fixing API route mismatches that prevent the Care Django plugin (`care_teleicu_devices`) from successfully controlling cameras.

The existing Django middleware works perfectly with the Care plugin. The Rust implementation must match the **exact API contract** that the Django plugin expects.

### Problem: API Route Path Mismatch

**What Django Plugin Expects** (`care_teleicu_devices/camera_device/viewsets/actions.py`):

```python
gateway_client.get("/status", request_data, as_http_response=True)
gateway_client.get("/presets", request_data, as_http_response=True)
gateway_client.post("/gotoPreset", request_data, as_http_response=True)
gateway_client.post("/absoluteMove", request_data, as_http_response=True)
gateway_client.post("/relativeMove", request_data, as_http_response=True)
gateway_client.post("/getToken/videoFeed", request_data, as_http_response=True)
```

**What Rust Currently Implements** (`src/main.rs`):

```rust
.route("/cameras/status", get(api::camera::get_camera_status))
.route("/cameras/presets", get(api::camera::get_presets))
.route("/cameras/absoluteMove", post(api::camera::absolute_move))
.route("/cameras/relativeMove", post(api::camera::relative_move))
.route("/cameras/gotoPreset", post(api::camera::goto_preset))
.route("/cameras/set_preset", post(api::camera::set_preset))
.route("/cameras/snapshotAtLocation", post(api::camera::snapshot_at_location))
```

**Issue**: Django calls `/status` but Rust expects `/cameras/status`. All camera control endpoints have an extra `/cameras` prefix that breaks the API contract.

### Root Cause

The Rust implementation was built based on `CameraArch.md` documentation but the route structure diverged from what the Django middleware actually implemented. The original Django middleware used routes **without** the `/cameras` prefix, and the Care plugin was built against that contract.

### Solution: Remove `/cameras` Prefix from Camera Routes

Change the Rust route registration to match Django middleware's API contract exactly.

### Changes Required

#### File: `src/main.rs`

**Current Routes**:
```rust
.route("/cameras/status", get(api::camera::get_camera_status))
.route("/cameras/presets", get(api::camera::get_presets))
.route("/cameras/absoluteMove", post(api::camera::absolute_move))
.route("/cameras/relativeMove", post(api::camera::relative_move))
.route("/cameras/gotoPreset", post(api::camera::goto_preset))
.route("/cameras/set_preset", post(api::camera::set_preset))
.route("/cameras/snapshotAtLocation", post(api::camera::snapshot_at_location))
```

**Fixed Routes**:
```rust
// Camera control endpoints (match Django middleware API contract)
.route("/status", get(api::camera::get_camera_status))
.route("/presets", get(api::camera::get_presets))
.route("/gotoPreset", post(api::camera::goto_preset))
.route("/absoluteMove", post(api::camera::absolute_move))
.route("/relativeMove", post(api::camera::relative_move))
.route("/set_preset", post(api::camera::set_preset))
.route("/snapshotAtLocation", post(api::camera::snapshot_at_location))

// Keep the "all cameras status" endpoint with /cameras prefix to avoid conflict
.route("/cameras/status", get(api::camera::cameras_status_all))
```

**Note**: The `/cameras/status` route for getting all cameras' status (used by monitoring) is kept separate with the prefix to avoid conflicting with the per-camera `/status` endpoint.

### Expected API Contract After Fix

| Method | Path | Purpose | Auth | Request Params |
|--------|------|---------|------|----------------|
| GET | `/status` | Get PTZ status for a camera | Care_Bearer | Query: `hostname`, `port`, `username`, `password` |
| GET | `/presets` | List presets for a camera | Care_Bearer | Query: `hostname`, `port`, `username`, `password` |
| POST | `/gotoPreset` | Go to preset by index | Care_Bearer | Body: `{hostname, port, username, password, preset: int}` |
| POST | `/absoluteMove` | Absolute PTZ move | Care_Bearer | Body: `{hostname, port, username, password, x, y, zoom}` |
| POST | `/relativeMove` | Relative PTZ move | Care_Bearer | Body: `{hostname, port, username, password, x, y, zoom}` |
| POST | `/set_preset` | Create new preset | Care_Bearer | Body: `{hostname, port, username, password, preset_name}` |
| POST | `/snapshotAtLocation` | Move & get snapshot URI | Care_Bearer | Body: `{hostname, port, username, password, x, y, zoom}` |
| GET | `/cameras/status` | All cameras' status | Care_Bearer | None (returns device statuses from observation store) |
| POST | `/getToken/videoFeed` | Get stream token | Care_Bearer | Body: `{stream, ip, _duration}` |
| POST | `/getToken/vitals` | Get vitals token | Care_Bearer | Body: `{asset_id, ip, _duration}` |
| POST | `/verifyToken` | Verify token | None | Body: `{token, ip?, stream?}` |
| POST | `/verify_token` | Exchange/verify token | None | Body: `{token}` |

### Testing Checklist

Verify drop-in replacement works:

- [ ] **Camera Status**: `GET /status?hostname=192.168.1.100&port=80&username=admin&password=admin` returns PTZ position
- [ ] **Presets List**: `GET /presets?hostname=192.168.1.100&port=80&username=admin&password=admin` returns preset map
- [ ] **Go to Preset**: `POST /gotoPreset` with `{hostname, port, username, password, preset: 0}` moves camera
- [ ] **Absolute Move**: `POST /absoluteMove` with `{hostname, port, username, password, x: 0.5, y: 0.0, zoom: 0.0}` moves camera
- [ ] **Relative Move**: `POST /relativeMove` with `{hostname, port, username, password, x: 0.1, y: 0.0, zoom: 0.0}` moves camera
- [ ] **Set Preset**: `POST /set_preset` with `{hostname, port, username, password, preset_name: "Test"}` creates preset
- [ ] **Snapshot**: `POST /snapshotAtLocation` with position params returns snapshot URI
- [ ] **All Cameras**: `GET /cameras/status` returns device status map (unchanged route)
- [ ] **Stream Token**: `POST /getToken/videoFeed` with `{stream, ip, _duration}` returns JWT
- [ ] **Django Plugin Integration**: Care UI camera controls work without any Django plugin changes

### Implementation Steps

1. **Update Routes in `src/main.rs`**:
   - Remove `/cameras` prefix from individual camera control endpoints
   - Keep `/cameras/status` for the "all cameras" monitoring endpoint
   - Verify no route conflicts (e.g., `/status` is unique)

2. **Test Locally**:
   ```bash
   # Start gateway
   cargo run
   
   # Test camera status endpoint
   curl "http://localhost:8000/status?hostname=192.168.1.100&port=80&username=admin&password=pass" \
     -H "Authorization: Bearer <care_jwt_token>"
   
   # Test presets endpoint
   curl "http://localhost:8000/presets?hostname=192.168.1.100&port=80&username=admin&password=pass" \
     -H "Authorization: Bearer <care_jwt_token>"
   ```

3. **Verify Django Plugin Compatibility**:
   - Deploy Rust gateway to test environment
   - Update Care's gateway device configuration to point to Rust gateway
   - Test camera controls from Care UI:
     - View camera status
     - List presets
     - Go to preset
     - Pan/tilt/zoom controls
     - Create new preset
   - Verify no errors in Care backend logs
   - Verify no errors in gateway logs

4. **Deploy as Drop-in Replacement**:
   - Stop Django middleware
   - Start Rust gateway on same port (default 8000)
   - No changes needed to Care configuration or database
   - Camera controls should work immediately

### Backwards Compatibility Notes

**Breaking Change**: Routes with `/cameras` prefix are **no longer supported** for individual camera operations.

**Migration Path**: None needed — the Django plugin has always used routes without the prefix. This fix makes Rust match the expected contract.

**Affected Routes**:
- ~~`/cameras/status`~~ → `/status` (per-camera status)
- ~~`/cameras/presets`~~ → `/presets`
- ~~`/cameras/absoluteMove`~~ → `/absoluteMove`
- ~~`/cameras/relativeMove`~~ → `/relativeMove`
- ~~`/cameras/gotoPreset`~~ → `/gotoPreset`

**Unchanged Routes**:
- `/cameras/status` (all cameras monitoring endpoint - different from per-camera status)
- `/getToken/videoFeed`
- `/health/*`
- `/update_observations`
- All WebSocket endpoints

### Additional Notes

1. **Port Configuration**: The Django plugin currently hardcodes `port: 80` in `get_gateway_request_data()`. Most ONVIF cameras use port 554 or 8000. This is a **Django plugin issue**, not a Rust issue. The Rust gateway correctly accepts any port via the API.

2. **Stream Token IP Field**: The Django plugin's `stream_token` action currently passes `metadata["endpoint_address"]` (camera IP) as the `ip` field, but it should pass the **client's IP** for proper access control. This is also a **Django plugin issue**.

3. **Camera Lock Mechanism**: The Rust implementation correctly implements camera locking with timeout. Multiple concurrent move operations will be serialized automatically.

4. **Error Responses**: Match Django middleware error responses:
   - Invalid credentials → `400 Bad Request` with `InvalidCameraCredentials` error
   - Camera locked → `409 Conflict` with `CameraLocked` error
   - ONVIF errors → `500 Internal Server Error` with error message

### Success Criteria

✅ **Drop-in Replacement Achieved When**:
1. Rust gateway runs on same port as Django middleware
2. Care Django plugin works without code changes
3. All camera control operations work from Care UI
4. Camera locking prevents concurrent operations
5. Stream tokens are generated correctly
6. No breaking changes to API contract
