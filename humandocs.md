# TeleICU Gateway — Architecture & Usage

## What This Is

A single Rust binary that runs on a hospital-edge device inside a TeleICU ward. It connects physical bedside devices (IP cameras, patient monitors, ventilators) to the CARE cloud platform used by remote clinicians.

---

## Usage

### Build

```sh
# via Cargo
cargo build --release
# binary at target/release/teleicu-gateway

# via Nix
nix build
# binary at result/bin/teleicu-gateway
```

### Configure

All config is via environment variables (or a `.env` file in the working directory). The key ones:

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `GATEWAY_DEVICE_ID` | **yes** | — | Unique ID for this gateway (registered in CARE) |
| `CARE_API` | no | `https://care.10bedicu.in` | CARE backend URL |
| `DATABASE_URL` | no | `sqlite:./gateway.db` | SQLite database path |
| `BIND_HOST` | no | `0.0.0.0` | Listen address |
| `BIND_PORT` | no | `8090` | Listen port |
| `JWKS_BASE64` | no | — | Pre-existing RSA keypair (base64-encoded). Auto-generated on first run if absent |
| `STATE_DIR` | no | `./data` | Directory for persisted state (keypair, etc.) |
| `RTSPTOWEB_URL` | no | `http://localhost:8080` | RTSPtoWeb instance to proxy stream routes to |
| `ENCRYPTION_KEY` | no | — | AES key for encrypting asset credentials at rest |
| `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `S3_BUCKET_NAME`, `S3_ENDPOINT_URL` | no | — | S3 archival (all four needed to enable) |
| `SENTRY_DSN` | no | — | Sentry error tracking |
| `RUST_LOG` | no | `info,teleicu_gateway=debug` | Log level filter |

### Run

```sh
# directly
GATEWAY_DEVICE_ID=my-ward-1 CARE_API=https://care.example.com ./teleicu-gateway

# or with a .env file
cp .env.example .env   # fill in values
./teleicu-gateway
```

The binary creates/migrates the SQLite database on startup and generates an RSA keypair into `STATE_DIR` if one doesn't exist. No other setup required.

### NixOS deployment

Import the flake's NixOS module and point it at an environment file:

```nix
services.teleicu-gateway = {
  enable = true;
  environmentFile = "/run/secrets/teleicu-gateway.env";
  rtsptowebConfigFile = ./rtsptoweb.json;
};
```

This creates two systemd services: `teleicu-gateway` and `rtsptoweb`.

---

## Four Subsystems

### 1. Camera Control (ONVIF)
Controls IP cameras over the hospital LAN using ONVIF (SOAP/XML over HTTP). Exposes a REST API for PTZ movement, presets, status polling, and snapshot URI fetching. Each camera request carries credentials directly — cameras are addressed by IP, not stored as sessions.

### 2. Observation Ingestion + Fan-out
Bedside patient monitors and ventilators POST vital signs to this gateway in real time (`POST /update_observations`). The gateway holds these observations in memory, fans them out to any connected WebSocket clients subscribed to that device's IP, and periodically ships them to CARE and to S3 for archival.

### 3. Stream Token Broker
Issues short-lived signed JWTs for video and vital streams. CARE requests a token, the gateway signs it with its own private key, and the token is used to authenticate WebSocket/HLS connections directly to `rtsptoweb`. The gateway also exposes a `verifyToken` endpoint that `rtsptoweb` or Nginx calls to validate those tokens.

### 4. Background Tasks
Three periodic jobs:
- **Automated observations** — reads recent in-memory observations, formats them as FHIR-ish records, POSTs to CARE's vitals endpoint.
- **Camera status sweep** — polls every ONVIF asset in the database for PTZ status, writes results to a status store that `/cameras/status` serves.
- **S3 dump** — ships stale observation data to an S3 bucket for long-term storage.

---

## Data Flow

```
Bedside Monitors / Ventilators
        │ POST /update_observations
        ▼
┌───────────────────────────────────────────────────────┐
│  teleicu-gateway  (port 8090)                         │
│                                                       │
│  In-memory observation store  ──► WebSocket clients   │
│  (ring buffer per device IP)      ws://.../observations/<ip>  │
│                                                       │
│  Asset DB (SQLite)            ──► background tasks    │
│  (ONVIF / HL7 / Ventilator        ├── observation ship│
│   assets with credentials)        ├── camera sweep    │
│                                   └── S3 dump         │
│                                                       │
│  ONVIF client (per-request)   ──► IP cameras (LAN)   │
└───────────────────────────────────────────────────────┘
        │                               │
   CARE Cloud API              rtsptoweb :8080
   (observations, health,       (RTSP → HLS/WebRTC)
    asset config)
```

---

## Trust / Auth Model

Two separate JWT trust relationships exist simultaneously:

**Inbound — CARE trusts the gateway:**
CARE sends requests with `Authorization: Care_Bearer <token>`. That token was issued by CARE's own auth system. The gateway validates it by fetching CARE's public key from `{CARE_API}/api/gateway_device/jwks.json/` (cached in memory).

**Outbound — CARE trusts the gateway to identify itself:**
The gateway has its own RSA key pair (the `JWKS_BASE64` env var). It signs its own JWTs and sends them as `Authorization: Gateway_Bearer <token>` plus `X-Gateway-Id: <device_id>` when calling CARE. CARE fetches the gateway's public key from `GET /openid-configuration/` to verify these.

**Stream tokens:**
Short-lived JWTs signed with the gateway's own private key. Contain `stream` + `ip` claims (video) or `asset_id` + `ip` (vitals). Verified at `POST /verifyToken`.

**WebSocket auth:**
Token passed in `sec-websocket-protocol` header, validated against the gateway's own public key.

---

## Asset Model

Three asset types live in the database:

| Type | What it is |
|---|---|
| `ONVIF` | IP camera — PTZ controllable, RTSP streamable |
| `HL7MONITOR` | Bedside patient monitor — pushes vitals observations |
| `VENTILATOR` | Ventilator — pushes observations |

All three share the same `Asset` table with fields: `id (UUID)`, `name`, `type`, `ip_address`, `port`, `username`, `password`, `access_key`, `description`, `deleted`.

---

## Observation Data Model

Observations are richly typed vital signs readings. Each has:
- `observation_id` — enum: `heart-rate`, `SpO2`, `blood-pressure`, `waveform`, `respiratory-rate`, `body-temperature1/2`, `pulse-rate`, `device-connection`, waveform variants
- `device_id` — the monitor's IP address
- `patient_id`, `patient_name`
- `status` — `final`, or message strings like `"Message-Leads Off"`, `"Connected"`, `"Disconnected"`
- Numeric value + unit + interpretation + reference range limits
- Blood pressure has nested systolic/diastolic/MAP sub-objects
- Waveform observations carry raw sample data

Validity rules: an observation is invalid if status signals a sensor problem (probe off, leads off, measurement error). Invalid observations are excluded from automated CARE uploads.

---

## Camera Locking

PTZ move operations take time. The system uses a per-camera lock (keyed by IP) that blocks concurrent move requests for up to 120 seconds. `AbsoluteMove`, `RelativeMove`, and `snapshotAtLocation` check the lock before proceeding. After issuing a move command, the controller polls `GetStatus` until `PanTilt == IDLE && Zoom == IDLE` before returning.

---

## ONVIF Notes

Cameras are addressed statelessly — every API call includes `hostname`, `port`, `username`, `password`. The controller creates a fresh ONVIF session per request. Operations:

`GetProfiles` → `GetStatus` → `AbsoluteMove` / `RelativeMove` / `GetPresets` / `GotoPreset` / `SetPreset` / `GetSnapshotUri`

Authentication uses WS-Security: each SOAP envelope includes a `<UsernameToken>` header with a timestamp, base64-encoded random nonce, and a SHA-1 password digest: `base64(SHA1(nonce_raw + created_utf8 + password_utf8))`.

---

## Nginx Routing (current, to be internalized)

| Path | Destination |
|---|---|
| `/start`, `/stream`, `/list`, `/stop` | rtsptoweb:8080 |
| `/logger` | gateway:8090 (WebSocket, system metrics) |
| `/update_observations` | **blocked (404)** — LAN-only |
| everything else | gateway:8090 |

In the Rust rewrite, all of this routing logic moves inside the binary. No Nginx.

---

## NixOS Target

One binary + one SQLite file. `rtsptoweb` runs as a separate systemd service in the same NixOS module. The gateway manages its own RSA keypair (generate on first boot, persist to `StateDirectory`). Zero Docker.
