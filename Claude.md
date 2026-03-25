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
reqwest        = { version = "0.11", features = ["json", "rustls-tls", "stream"], default-features = false }

# JWT — inbound validation
jsonwebtoken   = "9"

# RSA keypair — outbound signing + JWKS exposure
rsa            = { version = "0.9", features = ["pem", "sha2"] }
pkcs8          = { version = "0.10", features = ["alloc", "pem"] }

# JOSE / JWK serialization for /openid-configuration
p256           = "0.13"   # if EC keys needed later

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
config         = "0.14"

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

# Concurrent map for camera locks and observation store
dashmap        = "5"

# Async trait
async-trait    = "0.1"
```

---

## Configuration (`src/config.rs`)

Load with the `config` crate, `.env` file merged with environment. All fields required unless default noted.

```rust
pub struct Settings {
    pub bind_host: String,                   // default "0.0.0.0"
    pub bind_port: u16,                      // default 8090

    pub database_url: String,                // "sqlite:./gateway.db"

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
    pub app_version: String,                 // default "unknown"
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
