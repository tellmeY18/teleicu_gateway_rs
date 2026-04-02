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

### Docker deployment (current production setup)

RTSPtoWeb runs as a Docker container with the gateway running directly on the host:

```yaml
# docker-compose.yml (or equivalent docker run)
services:
  stream-server:
    restart: always
    image: ghcr.io/deepch/rtsptoweb:latest
    volumes:
      - ./RtspConfig.json:/config/config.json
    ports:
      - "8080:8080"
```

The gateway binary runs on the host and proxies requests to `http://localhost:8080`:

```sh
# Set the RTSPtoWeb URL to point to the Docker container
export RTSPTOWEB_URL=http://localhost:8080
export GATEWAY_DEVICE_ID=my-ward-1
export CARE_API=https://care.example.com

# Run the gateway
./teleicu-gateway
```

**RtspConfig.json structure:**
The config file must use UUIDs as stream identifiers and follow this schema:
```json
{
  "streams": {
    "<uuid>": {
      "name": "human-readable-name",
      "channels": {
        "0": {
          "on_demand": true,
          "url": "rtsp://username:password@camera-ip"
        }
      }
    }
  }
}
```

The UUID in the config must match the `stream` parameter in `/getToken/videoFeed` requests from CARE.

### Testing Docker deployment

After starting the RTSPtoWeb container and gateway, verify the setup:

**1. Check RTSPtoWeb is running:**
```sh
docker ps
# Should show ghcr.io/deepch/rtsptoweb:latest running on port 8080

curl http://localhost:8080/list
# Should return JSON with your configured streams
```

**2. Test stream availability:**
```sh
# Replace UUID with one from your RtspConfig.json
curl "http://localhost:8080/stream/81087826-1e10-4a50-a4ec-ab0064d34745/channel/0/webrtc"
# Should return stream metadata (not an error)
```

**3. Verify gateway can reach RTSPtoWeb:**
```sh
# Check gateway logs for:
# "RTSPTOWEB_URL = http://localhost:8080"

# Test proxy routes through gateway
curl http://localhost:8090/list
# Should return the same stream list as step 1
```

**4. Test ONVIF camera control:**
```sh
# Get JWT token from CARE first, then:
curl -X GET "http://localhost:8090/cameras/status?hostname=192.168.68.65&port=80&username=remoteuser&password=test" \
  -H "Authorization: Care_Bearer <your_care_token>"
# Should return camera PTZ status
```

**5. Test stream token generation:**
```sh
curl -X POST http://localhost:8090/getToken/videoFeed \
  -H "Authorization: Care_Bearer <your_care_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "ip": "192.168.68.65",
    "stream": "81087826-1e10-4a50-a4ec-ab0064d34745",
    "duration": "5"
  }'
# Should return: {"token": "<jwt_string>"}
```

**Common issues:**

- **"connection refused" on port 8080:** RTSPtoWeb container isn't running or port mapping is wrong
- **Empty stream list:** `RtspConfig.json` not mounted correctly or has syntax errors
- **Camera not connecting:** Check RTSP URL format, credentials, and camera network accessibility
- **Token verification fails:** Ensure `token.backend` in RTSPtoWeb config points to gateway's `/verifyToken` endpoint

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

### 1. Camera Control (ONVIF) & Streaming (RTSP)

The camera subsystem has two independent but coordinated layers:

#### Layer 1: ONVIF Control (Pan-Tilt-Zoom)

Controls IP cameras over the hospital LAN using ONVIF (SOAP/XML over HTTP). Exposes a REST API for PTZ movement, presets, status polling, and snapshot URI fetching.

**Key characteristics:**
- **Stateless per-request authentication** — every API call includes `hostname`, `port`, `username`, `password` in the request body or query params. No session state.
- **No asset database dependency for control** — CARE sends credentials with each request; the gateway doesn't need the camera pre-registered to move it.
- **WS-Security authentication** — each SOAP envelope includes a `<UsernameToken>` header with timestamp, nonce, and SHA-1 password digest.

**API endpoints:**

| Endpoint | Auth | Method | Purpose |
|---|---|---|---|
| `GET /cameras/presets` | Care_Bearer | GET | List all presets for a camera (returns `{ "presets": { "name": index } }`) |
| `GET /cameras/status` | Care_Bearer | GET | Get current PTZ position and move status for one camera |
| `POST /cameras/absoluteMove` | Care_Bearer | POST | Move to absolute position `(x, y, zoom)` in range `[-1.0, 1.0]` |
| `POST /cameras/relativeMove` | Care_Bearer | POST | Move relative to current position by `(dx, dy, dzoom)` |
| `POST /cameras/gotoPreset` | Care_Bearer | POST | Move to a saved preset by numeric index |
| `POST /cameras/set_preset` | Care_Bearer | POST | Save current position as a new preset with given name |
| `POST /cameras/snapshotAtLocation` | Care_Bearer | POST | Move to position, wait for idle, return snapshot URI |
| `GET /cameras/status` (all) | Care_Bearer | GET | Return device connection status for all cameras in the observation store |

**Request flow example (absolute move):**

```
1. CARE → POST /cameras/absoluteMove
   Body: {
     "hostname": "192.168.1.100",
     "port": 80,
     "username": "admin",
     "password": "camera123",
     "x": 0.5, "y": -0.3, "zoom": 0.0
   }

2. Gateway checks camera lock (per-IP, 120s timeout)
   → If locked, return 423 Locked immediately
   → If free, acquire lock

3. Gateway creates OnvifClient with credentials

4. GetProfiles SOAP call → extract profile_token

5. AbsoluteMove SOAP call with (x, y, zoom)

6. Poll GetStatus every 500ms until:
   PanTilt == "IDLE" && Zoom == "IDLE"
   (max wait: camera_lock_timeout_secs, default 120s)

7. Release lock, return { "status": "ok" }
```

**Camera locking:**
PTZ operations are inherently sequential — a camera can't execute two moves simultaneously. The gateway uses a per-camera lock (keyed by IP address) that:
- Blocks concurrent move requests to the same camera
- Times out after 120 seconds (configurable via `camera_lock_timeout_secs`)
- Applies to: `absoluteMove`, `relativeMove`, `gotoPreset`, `snapshotAtLocation`
- Does NOT apply to: `getPresets`, `getStatus` (read-only ops)

**ONVIF operation sequence:**
Every control operation follows this pattern:
1. Create SOAP envelope with WS-Security header (username token + digest)
2. POST to `http://{hostname}:{port}/onvif/device_service` (or `/ptz_service`)
3. Parse XML response
4. Extract data from SOAP body

The gateway uses the `quick-xml` crate to parse responses. Invalid credentials return HTTP 401 or a SOAP fault with "NotAuthorized" — both are mapped to `AppError::InvalidCameraCredentials`.

#### Layer 2: RTSP Streaming (via RTSPtoWeb)

Video streams are served by [RTSPtoWeb](https://github.com/deepch/RTSPtoWeb), a separate process that converts RTSP streams to HLS/WebRTC for browser consumption.

**Architecture:**

```
IP Camera (RTSP server)
    │ rtsp://192.168.1.100:554/stream1
    │ (H.264/H.265 video + audio)
    ▼
┌──────────────────────────────┐
│  RTSPtoWeb  (:8080)          │
│  - Reads RTSP stream         │
│  - Transcodes to HLS/WebRTC  │
│  - Serves /stream, /start,   │
│    /stop, /list endpoints    │
└──────────────────────────────┘
    │ Proxied through gateway
    ▼
┌──────────────────────────────┐
│  teleicu-gateway  (:8090)    │
│  Reverse proxy routes:       │
│  /start → rtsptoweb:8080     │
│  /stream → rtsptoweb:8080    │
│  /list → rtsptoweb:8080      │
│  /stop → rtsptoweb:8080      │
└──────────────────────────────┘
    │
    ▼
CARE / Browser
```

**RTSP URL format:**
Cameras expose RTSP streams at URLs like:
```
rtsp://{username}:{password}@{hostname}:{port}/{stream_path}
```

Example:
```
rtsp://admin:camera123@192.168.1.100:554/stream1
rtsp://admin:camera123@192.168.1.100:554/Streaming/Channels/101
```

The `{stream_path}` varies by camera manufacturer (Hikvision, Dahua, Axis, etc.). Typically:
- Hikvision: `/Streaming/Channels/101` (main), `/Streaming/Channels/102` (sub)
- Dahua: `/cam/realmonitor?channel=1&subtype=0` (main), `subtype=1` (sub)
- Generic: `/stream1`, `/stream2`

**RTSPtoWeb configuration:**
RTSPtoWeb needs a JSON config file (`RtspConfig.json`) listing all cameras and their RTSP URLs. Each stream is identified by a UUID and contains one or more channels:

```json
{
  "channel_defaults": {
    "on_demand": true
  },
  "server": {
    "http_port": ":8080",
    "log_level": "info",
    "token": {
      "backend": "",
      "enable": false
    }
  },
  "streams": {
    "81087826-1e10-4a50-a4ec-ab0064d34745": {
      "channels": {
        "0": {
          "on_demand": true,
          "url": "rtsp://remoteuser:test@192.168.68.65"
        }
      },
      "name": "camera1"
    },
    "6b1a2814-d1d7-4af4-97f2-30cf2b304513": {
      "channels": {
        "0": {
          "on_demand": true,
          "url": "rtsp://remoteuser:test@192.168.68.66"
        }
      },
      "name": "camera2"
    },
    "a077357e-a4c6-4ef5-b2d3-c85117521728": {
      "channels": {
        "0": {
          "on_demand": true,
          "url": "rtsp://remoteuser:test@192.168.68.67"
        }
      },
      "name": "camera3"
    }
  }
}
```

**Key configuration notes:**
- Stream UUIDs can be generated with `uuidgen` or any UUID v4 generator
- The `on_demand` setting keeps connections closed until a client requests the stream (saves camera resources)
- Multiple channels (0, 1, 2...) can be defined per stream for different quality levels
- The `name` field is human-readable but the UUID is what's used in API calls

**⚠️ Security Warning:**
The example above has `"token": { "enable": false }` which disables token verification in RTSPtoWeb. This is **NOT secure for production**. For production deployments, you should enable token verification:

```json
"token": {
  "enable": true,
  "backend": "http://localhost:8090/verifyToken"
}
```

With this configuration, RTSPtoWeb will call the gateway's `/verifyToken` endpoint to validate every stream request. Without this, anyone with network access to port 8080 can view camera streams without authentication.

In Docker deployments, this file is mounted as a volume. In NixOS deployments, it's provided via `services.teleicu-gateway.rtsptowebConfigFile`.

**Mapping physical cameras to stream UUIDs:**

When setting up cameras, you need to:

1. **Generate a UUID for each camera:**
   ```sh
   uuidgen  # e.g., 81087826-1e10-4a50-a4ec-ab0064d34745
   ```

2. **Add the camera to RtspConfig.json** with its RTSP URL and the generated UUID

3. **Register the camera in CARE** with the same UUID as the stream identifier

4. **Record the mapping** for your ward:
   ```
   Bed 1 Camera → UUID: 81087826-1e10-4a50-a4ec-ab0064d34745 → IP: 192.168.68.65
   Bed 2 Camera → UUID: 6b1a2814-d1d7-4af4-97f2-30cf2b304513 → IP: 192.168.68.66
   Bed 3 Camera → UUID: a077357e-a4c6-4ef5-b2d3-c85117521728 → IP: 192.168.68.67
   ```

When CARE requests a video stream for "Bed 1," it sends the UUID `81087826-1e10-4a50-a4ec-ab0064d34745` in the `/getToken/videoFeed` request. The gateway issues a token containing this UUID, and RTSPtoWeb uses it to look up the RTSP URL from its config.

**ONVIF vs RTSP addressing:**
- **ONVIF control** uses IP addresses directly (CARE sends `"hostname": "192.168.68.65"` in PTZ requests)
- **RTSP streaming** uses UUIDs (CARE sends `"stream": "81087826-1e10-4a50-a4ec-ab0064d34745"`)
- The same physical camera has both an IP (for control) and a UUID (for streaming)

**Stream token flow:**

```
1. CARE requests a stream token:
   POST /getToken/videoFeed
   Authorization: Care_Bearer <care_token>
   Body: {
     "ip": "192.168.68.65",
     "stream": "81087826-1e10-4a50-a4ec-ab0064d34745",
     "duration": "10"  // minutes
   }

2. Gateway validates Care_Bearer token (fetches CARE's JWKS)

3. Gateway signs its own JWT with:
   - Claims: { "stream": "81087826-1e10-4a50-a4ec-ab0064d34745", "ip": "192.168.68.65" }
   - Expiry: 10 minutes (clamped to 1–60 min)
   - Signed with gateway's private RSA key

4. Gateway returns: { "token": "<gateway_jwt>" }

5. CARE passes this token to the browser

6. Browser requests stream:
   GET /stream?uuid=81087826-1e10-4a50-a4ec-ab0064d34745&token=<gateway_jwt>
   (proxied to RTSPtoWeb by the gateway)

7. RTSPtoWeb (or Nginx in some setups) calls back to verify:
   POST /verifyToken
   Body: { "token": "<gateway_jwt>", "stream": "81087826-1e10-4a50-a4ec-ab0064d34745" }

8. Gateway verifies JWT signature + checks claims match
   Returns: { "status": "1" } (valid) or { "status": "0" (invalid)

9. If valid, RTSPtoWeb serves HLS/WebRTC stream to browser
```

**Why two separate systems?**
- **ONVIF** = control plane (move camera, adjust zoom, save presets)
- **RTSP** = data plane (video/audio streaming)

They're independent protocols. A camera can support ONVIF for PTZ but stream via RTSP. The gateway handles ONVIF directly (Rust SOAP client), but delegates RTSP → HLS transcoding to RTSPtoWeb (written in Go, optimized for video).

**Common workflow (CARE requesting a video feed):**

1. **CARE**: "Show me bed 3's camera at preset 'Ventilator View'"
2. **Gateway**: Move camera via ONVIF `POST /cameras/gotoPreset`
3. **Gateway**: Issue stream token `POST /getToken/videoFeed`
4. **CARE**: Display video player with token
5. **Browser**: Request stream `/stream?uuid=...&token=...` (proxied through gateway)
6. **RTSPtoWeb**: Verify token, serve HLS chunks
7. **Browser**: Play video

**Security notes:**
- RTSP credentials (username/password) are embedded in the RTSPtoWeb config file, not sent over the network per-request
- Stream tokens are short-lived (default 5 min, max 60 min)
- Tokens are signed by the gateway's own RSA key (not CARE's key)
- RTSPtoWeb doesn't trust tokens directly — it calls back to `/verifyToken` to validate them
- ONVIF credentials ARE sent per-request (CARE has them, gateway is stateless)


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
                           Hospital LAN
                                │
        ┌───────────────────────┼───────────────────────┐
        │                       │                       │
  IP Cameras                Monitors              Ventilators
  (ONVIF + RTSP)           (HL7/HTTP)            (HTTP)
        │                       │                       │
        │ ONVIF/SOAP           │ POST                  │ POST
        │ PTZ control          │ /update_observations  │ /update_observations
        │                       │                       │
        │                       ▼                       ▼
        │         ┌─────────────────────────────────────────────┐
        │         │  teleicu-gateway  (port 8090)               │
        │         │                                             │
        │         │  ┌─ ONVIF Client (stateless, per-request) │
        └────────►│  │  • absoluteMove, relativeMove           │
                  │  │  • getPresets, gotoPreset               │
 RTSP stream      │  │  • Camera locks (per-IP)                │
 (not via gateway)│  │                                         │
        │         │  ┌─ Observation Store (in-memory)          │
        │         │  │  • Ring buffers per device IP           │
        │         │  │  • WebSocket fan-out                    │
        │         │  │  • Device status tracking               │
        │         │  │                                         │
        │         │  ┌─ Background Tasks                       │
        │         │  │  ├── Automated observations → CARE      │
        │         │  │  ├── Camera status sweep                │
        │         │  │  └── S3 archival dump                   │
        │         │  │                                         │
        │         │  ┌─ Stream Token Broker                    │
        │         │  │  • Signs JWTs for video/vitals          │
        │         │  │  • /verifyToken endpoint                │
        │         │  │                                         │
        │         │  ┌─ Reverse Proxy                          │
        │         │  │  • /stream, /start, /list, /stop        │
        │         │  │  → forwards to RTSPtoWeb                │
        │         └──┴──────────────────────────────────────────┘
        │                   │                   │
        │                   │ HTTPS             │ WebSocket
        │                   │                   │ (observations)
        │                   ▼                   ▼
        │            CARE Cloud API         CARE Frontend
        │            • Observation POST     • ws://.../observations/<ip>
        │            • Asset config         • ws://.../logger
        │            • Health checks
        │
        ▼
  ┌──────────────────────┐
  │ RTSPtoWeb  (:8080)   │
  │ • RTSP → HLS/WebRTC  │
  │ • Token verification │
  │   via /verifyToken   │
  └──────────────────────┘
        │
        │ HLS/WebRTC
        ▼
   Browser Video Player
```

**Key flows:**

1. **Camera Control (ONVIF)**: CARE → Gateway SOAP client → Camera (stateless, credentials per-request)
2. **Video Streaming (RTSP)**: Camera → RTSPtoWeb → Browser (gateway proxies /stream routes + issues/verifies tokens)
3. **Vitals Ingestion**: Monitor/Vent → Gateway observation store → WebSocket subscribers + CARE API + S3
4. **Stream Auth**: CARE → Gateway `/getToken` → Browser → RTSPtoWeb → Gateway `/verifyToken` → Stream served

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

## Demo with HTTPS

For demos or local development where HTTPS is required (e.g. CARE expects `https://`), the flake includes a one-command launcher that runs the gateway behind [Caddy](https://caddyserver.com/) with an auto-generated self-signed certificate.

### One command (Nix builds everything)

```sh
nix run .#demo
```

This builds the gateway from source, starts it on `http://127.0.0.1:8090`, and launches Caddy as an HTTPS reverse proxy on `https://localhost:8443`. No Rust toolchain, no Caddy install, no cert generation — Nix handles all of it.

### From the dev shell (if you're already iterating)

```sh
nix develop .#demo
./dev/demo.sh
```

This uses the `demo` dev shell (which includes Caddy + curl alongside the Rust toolchain) and runs the same launcher script, except it uses your local `cargo build --release` binary instead of building via Nix.

### Configuration

| Variable | Default | Purpose |
|---|---|---|
| `DEMO_PORT` | `8443` | HTTPS port Caddy listens on |
| `BIND_PORT` | `8090` | HTTP port the gateway listens on (internal) |
| `GATEWAY_DEVICE_ID` | `demo-gateway` | Auto-set if not provided |

All other gateway env vars (`.env` file, `CARE_API`, etc.) work as normal.

### What it does

```
Browser / CARE
    │ https://localhost:8443
    ▼
┌─────────────────────────┐
│  Caddy (self-signed TLS)│
│  :8443 → localhost:8090 │
└────────────┬────────────┘
             │ http
             ▼
┌─────────────────────────┐
│  teleicu-gateway        │
│  :8090 (plain HTTP)     │
└─────────────────────────┘
```

Caddy generates a locally-trusted self-signed certificate on first run. Your browser will show a security warning — accept it to proceed. The gateway binary itself is unchanged; TLS terminates entirely at Caddy.

Press `Ctrl+C` to stop both processes.

> **Production note:** This is for demos only. In production, use the NixOS module (which can sit behind a real reverse proxy with proper certs) or terminate TLS at your infrastructure layer.

---

## Getting the Binary

### Option 1: Build locally with Nix (recommended)

The flake produces a fully reproducible binary. No Rust toolchain needed on your machine — Nix handles everything:

```sh
nix build github:10BedICU/teleicu_gateway_rs
# binary at ./result/bin/teleicu-gateway
```

Or from a local checkout:

```sh
nix build
./result/bin/teleicu-gateway --help
```

To copy the built binary to a remote machine:

```sh
# copy the Nix store path to a remote NixOS host
nix copy --to ssh://root@ward-device .#default

# or just scp the binary directly
scp result/bin/teleicu-gateway root@ward-device:/usr/local/bin/
```

### Option 2: Build locally with Cargo

Requires the Rust toolchain, SQLite, and (on macOS) libiconv. Use the Nix dev shell to get all dependencies:

```sh
nix develop            # enter dev shell with all native deps
cargo build --release  # binary at target/release/teleicu-gateway
```

> **macOS note:** Always use `nix develop` before `cargo build`. The project depends on `libiconv` and `sqlite` which the flake's dev shell provides. Building outside the shell will fail with linker errors.

### Option 3: NixOS module (production deployments)

For NixOS target machines, skip the binary entirely — import the flake's NixOS module and let systemd manage everything:

```nix
services.teleicu-gateway = {
  enable = true;
  environmentFile = "/run/secrets/teleicu-gateway.env";
  rtsptowebConfigFile = ./rtsptoweb.json;
};
```

The module builds the binary from source as part of the NixOS system closure. Running `nixos-rebuild switch` on the target machine builds, deploys, and restarts the service in one step.

### What about CI on Tangled?

The repository has Spindle CI workflows (`.tangled/workflows/`) that build and test on every push to `main`. These verify the binary compiles and tests pass, but **Spindle does not currently support artifact downloads** — there's no equivalent of GitHub Releases or `actions/upload-artifact`. The build container's filesystem is ephemeral.

What the CI *does* do:
- **`ci.yml`** — runs `cargo fmt --check`, `cargo clippy`, and `cargo test` on every push/PR
- **`build.yml`** — runs `cargo build --release`, strips the binary, and logs the size + SHA256

You can view pipeline results on the repository's pipelines page on Tangled.org, but to actually get a binary, use one of the three options above.

---

## NixOS Target

One binary + one SQLite file. `rtsptoweb` runs as a separate systemd service in the same NixOS module. The gateway manages its own RSA keypair (generate on first boot, persist to `StateDirectory`). Zero Docker.
