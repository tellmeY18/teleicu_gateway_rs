# Camera Architecture Documentation - TeleICU Gateway

## Table of Contents

1. [Overview](#overview)
2. [Architecture Components](#architecture-components)
3. [RTSP Stream Management](#rtsp-stream-management)
4. [ONVIF Camera Control](#onvif-camera-control)
5. [Authentication & Authorization](#authentication--authorization)
6. [WebSocket Communication](#websocket-communication)
7. [Redis State Management](#redis-state-management)
8. [Camera Locking Mechanism](#camera-locking-mechanism)
9. [API Endpoints Reference](#api-endpoints-reference)
10. [Data Flow Diagrams](#data-flow-diagrams)
11. [Database Models](#database-models)
12. [Rust Implementation Guide](#rust-implementation-guide)

---

## Overview

The TeleICU Gateway middleware manages RTSP camera streams and ONVIF camera control for ICU monitoring systems. The system separates concerns:

- **Django Middleware**: Handles ONVIF camera control (PTZ, presets, snapshots)
- **RTSPtoWeb Service**: Manages RTSP stream conversion to WebRTC/HLS
- **Nginx Reverse Proxy**: Routes requests and enforces rate limiting
- **Redis**: Manages camera state, locks, and observation queues
- **PostgreSQL**: Persists asset configuration and daily rounds

### Key Features

1. **ONVIF Protocol Support**: Pan/Tilt/Zoom control via ONVIF SOAP APIs
2. **Stream Tokenization**: JWT-based access control for video streams
3. **Camera Locking**: Prevents concurrent PTZ operations
4. **Status Monitoring**: Periodic camera health checks via Celery
5. **WebSocket Observations**: Real-time device data streaming
6. **Movement Completion Detection**: Polls camera status until PTZ operations complete

---

## Architecture Components

### Service Topology

```
┌─────────────────────────────────────────────────────────────────┐
│                        Nginx Reverse Proxy                       │
│                         (Port 8001)                              │
│  - Rate Limiting (20 req/s)                                      │
│  - WebSocket Upgrade Support                                     │
└───────────┬─────────────────────────────────┬───────────────────┘
            │                                 │
            ├─ /stream/*  ─────────┐          ├─ /*  ──────────┐
            ├─ /start               │          ├─ /logger       │
            ├─ /stop                │          └─ /observations │
            └─ /list                │                           │
                                    ▼                           ▼
            ┌───────────────────────────────┐  ┌─────────────────────────────┐
            │   RTSPtoWeb Service           │  │  Django Middleware          │
            │   (Port 8080)                 │  │  (Port 8090)                │
            │   - RTSP to WebRTC/HLS        │  │  - ONVIF Camera Control     │
            │   - Stream Management         │  │  - Token Generation         │
            │   - ghcr.io/deepch/rtsptoweb  │  │  - WebSocket Server         │
            └───────────────────────────────┘  │  - REST API Endpoints       │
                                               └─────────┬───────────────────┘
                                                         │
                    ┌────────────────────────────────────┼────────────────────┐
                    │                                    │                    │
                    ▼                                    ▼                    ▼
        ┌───────────────────┐              ┌─────────────────────┐  ┌─────────────────┐
        │   Redis Cache     │              │   PostgreSQL DB     │  │  Celery Worker  │
        │   - Camera Locks  │              │   - Assets          │  │  - Health Checks│
        │   - Statuses      │              │   - Daily Rounds    │  │  - Scheduled    │
        │   - Observations  │              │   - Configuration   │  │    Tasks        │
        └───────────────────┘              └─────────────────────┘  └─────────────────┘
```

### Docker Compose Services

1. **stream-server**: `ghcr.io/deepch/rtsptoweb:latest`
   - External service handling RTSP → WebRTC/HLS conversion
   - Requires `RtspConfig.json` configuration (not in repo, runtime generated)
   - Port: 8080

2. **teleicu-middleware**: Django/Daphne ASGI server
   - ONVIF camera control
   - JWT token generation for streams
   - WebSocket server for observations
   - Port: 8090

3. **reverse-proxy**: Nginx
   - Routes `/stream/*` to stream-server
   - Routes all other requests to teleicu-middleware
   - Rate limiting: 20 req/s per IP
   - Port: 8001 (external)

4. **celery**: Background task processor
   - `store_camera_statuses`: Periodic camera health checks
   - `automated_observations`: Scheduled observation collection

5. **redis**: Cache and message broker
   - Camera lock keys: `CAMERA_LOCK_KEY{ip}` (TTL: 120s)
   - Status keys: `camera_statuses_{timestamp}`
   - Observation queues: `observations_{timestamp}`

6. **db**: PostgreSQL 17
   - Asset persistence
   - Daily round logs

---

## RTSP Stream Management

### Stream Flow Architecture

The Django middleware **does NOT directly handle RTSP streams**. Instead:

1. **External RTSPtoWeb Service** manages RTSP connections
2. **Django generates JWT tokens** for stream access authorization
3. **Nginx proxies stream requests** to RTSPtoWeb service

### RTSPtoWeb Integration

**Service**: `ghcr.io/deepch/rtsptoweb:latest`

**Configuration**: `RtspConfig.json` (mounted at `/config/config.json`)
- Not in repository
- Typically contains stream mappings: UUID → RTSP URL
- Example structure (inferred):
```json
{
  "streams": {
    "stream-uuid-1": {
      "url": "rtsp://username:password@camera-ip:554/stream1",
      "on_demand": true
    }
  }
}
```

### Stream Access Flow

```
1. Client requests stream token
   POST /getToken/videoFeed
   {
     "stream": "camera-uuid",
     "ip": "client-ip",
     "_duration": "5"  // minutes
   }

2. Django generates JWT token
   - Claims: {"stream": "camera-uuid", "ip": "client-ip"}
   - Expiry: duration * 60 seconds (max 60 minutes)
   - Signed with JWKS (RS256 algorithm)

3. Client connects to stream with token
   WS /stream/camera-uuid?token=<jwt>

4. Nginx forwards to RTSPtoWeb (port 8080)

5. RTSPtoWeb validates token and serves WebRTC/HLS stream
```

### Stream Token Generation

**File**: `middleware/stream/views.py`

```python
class MiddlewareStreamViewSet(viewsets.ViewSet):
    @action(detail=False, methods=["post"], url_path="getToken/videoFeed")
    def get_video_feed_stream_token(self, request):
        request = VideoStreamRequest.model_validate(request.data)
        duration = int(request._duration if request._duration else "5")
        
        # Validation: 0 < duration <= 60 minutes
        if duration < 0 or duration > 60:
            return Response({"message": "duration must be between 0 and 60"}, status=400)
        
        middleware_token = generate_jwt(
            claims={"stream": request.stream, "ip": request.ip}, 
            exp=60 * duration
        )
        
        return Response({"token": {middleware_token}}, status=200)
```

### Token Verification Endpoint

**Purpose**: Allows RTSPtoWeb or other services to verify JWT tokens

**Endpoint**: `POST /verifyToken`

```python
def validate_stream_token(self, request):
    request = VerifyStreamTokenRequest.model_validate(request.data)
    
    # Decode JWT using JWKS public key
    key = settings.JWKS.as_dict()["keys"][0]
    public_key = jwt.algorithms.RSAAlgorithm.from_jwk(key)
    value = jwt.decode(request.token, key=public_key, algorithms=["RS256"])
    
    decoded_value = VideoStreamRequest.model_validate(value)
    
    # Verify IP and stream match
    if decoded_value.ip == request.ip or decoded_value.stream == request.stream:
        return Response({"status": "1"}, status=200)
    
    return Response({"status": "0"}, status=401)
```

### Nginx Stream Routing

**File**: `nginx/nginx.conf`

```nginx
# Stream endpoints (proxied to RTSPtoWeb)
location /stream {
    limit_req zone=ip burst=12 delay=8;
    proxy_pass http://stream-server:8080;
    
    # WebSocket upgrade headers
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "Upgrade";
    
    # Disable caching for live streams
    proxy_cache off;
    add_header 'Cache-Control' 'no-store, no-cache, must-revalidate';
}

location /start {
    proxy_pass http://stream-server:8080;
}

location /stop {
    proxy_pass http://stream-server:8080;
}

location /list {
    proxy_pass http://stream-server:8080;
}
```

### Important Notes for Rust Implementation

1. **No Direct RTSP Handling**: Your Rust implementation should NOT implement RTSP protocol
2. **Keep RTSPtoWeb**: Continue using the external `rtsptoweb` Docker service
3. **Implement JWT Generation**: Use `jsonwebtoken` crate with RS256 algorithm
4. **Proxy Configuration**: Ensure Nginx or equivalent routes `/stream/*` to RTSPtoWeb
5. **JWKS Management**: Load/generate JWKS on startup from environment variable

---

## ONVIF Camera Control

### ONVIF Protocol Overview

**ONVIF** (Open Network Video Interface Forum) uses SOAP-based web services over HTTP/HTTPS.

**Python Implementation**: Uses `onvif-zeep` library
- WSDL files: `/path/to/onvif-zeep/wsdl/` (mounted from Python package)
- Protocols: Media, PTZ, DeviceManagement

### Camera Controller Architecture

**Abstract Base**: `middleware/camera/abstract_camera.py`

```python
class AbstractCameraController(ABC):
    @abstractmethod
    def go_to_preset(self, preset_id: str):
        pass

    @abstractmethod
    def get_presets(self, req: CameraAsset):
        pass

    @abstractmethod
    def get_status(self, req: CameraAsset):
        pass

    @abstractmethod
    def absolute_move(self, pan: float, tilt: float, zoom: float):
        pass

    @abstractmethod
    def relative_move(self, pan: float, tilt: float, zoom: float):
        pass

    @abstractmethod
    def set_preset(self, preset_name: str):
        pass

    @abstractmethod
    def get_snapshot_uri(self):
        pass
```

**Concrete Implementation**: `middleware/camera/onvif_zeep_camera_controller.py`

### Camera Initialization

```python
class OnvifZeepCameraController(AbstractCameraController):
    def __init__(self, req: CameraAsset) -> None:
        # CameraAsset: hostname, port, username, password
        
        cam = ONVIFCamera(
            req.hostname, 
            req.port, 
            req.username, 
            req.password, 
            settings.WSDL_PATH  # Path to ONVIF WSDL files
        )
        
        # Create service objects
        media = cam.create_media_service()
        ptz = cam.create_ptz_service()
        
        # Get first media profile (cameras typically have 1-2 profiles)
        media_profile = media.GetProfiles()[0]
        
        # Store references
        self.cam = cam
        self.camera_ptz = ptz
        self.camera_media_profile = media_profile
        self.camera_media = media
```

**Error Handling**:
- Catches `ONVIFError` on connection failure
- Raises `InvalidCameraCredentialsException` if "credentials" in error message
- Re-raises other errors

### ONVIF Operations

#### 1. Get Camera Status

**ONVIF Method**: `PTZ.GetStatus`

```python
def get_status(self):
    request = self.camera_ptz.create_type("GetStatus")
    request.ProfileToken = self.camera_media_profile.token
    ptz_status = self.camera_ptz.GetStatus(request)
    
    # Extract position
    pan = ptz_status.Position.PanTilt.x
    tilt = ptz_status.Position.PanTilt.y
    zoom = ptz_status.Position.Zoom.x
    
    # Extract movement status
    pan_tilt_status = ptz_status.MoveStatus.PanTilt  # "IDLE" or "MOVING"
    zoom_status = ptz_status.MoveStatus.Zoom
    
    # Error handling
    error = ptz_status.Error
    if error and error.lower().replace(" ", "") in ("noerror",):
        error = None
    
    return {
        "position": {"x": pan, "y": tilt, "zoom": zoom},
        "moveStatus": {"panTilt": pan_tilt_status, "zoom": zoom_status},
        "error": error
    }
```

**Response Structure**:
```json
{
  "position": {
    "x": 0.5,      // Pan: -1.0 to 1.0
    "y": 0.2,      // Tilt: -1.0 to 1.0
    "zoom": 0.1    // Zoom: 0.0 to 1.0
  },
  "moveStatus": {
    "panTilt": "IDLE",  // "IDLE" or "MOVING"
    "zoom": "IDLE"
  },
  "error": null
}
```

#### 2. Absolute Move

**ONVIF Method**: `PTZ.AbsoluteMove`

```python
@wait_for_movement_completion  # Decorator blocks until movement finishes
def absolute_move(self, pan: float, tilt: float, zoom: float):
    request = self.camera_ptz.create_type("AbsoluteMove")
    request.ProfileToken = self.camera_media_profile.token
    request.Position = {
        "PanTilt": {"x": pan, "y": tilt}, 
        "Zoom": zoom
    }
    resp = self.camera_ptz.AbsoluteMove(request)
    return resp
```

**Parameters**:
- `pan`: Absolute pan position (-1.0 to 1.0)
- `tilt`: Absolute tilt position (-1.0 to 1.0)
- `zoom`: Absolute zoom level (0.0 to 1.0)

#### 3. Relative Move

**ONVIF Method**: `PTZ.RelativeMove`

```python
@wait_for_movement_completion
def relative_move(self, pan: float, tilt: float, zoom: float):
    request = self.camera_ptz.create_type("RelativeMove")
    request.ProfileToken = self.camera_media_profile.token
    request.Translation = {
        "PanTilt": {"x": pan, "y": tilt}, 
        "Zoom": zoom
    }
    resp = self.camera_ptz.RelativeMove(request)
    return resp
```

**Parameters**:
- `pan`: Relative pan delta (-1.0 to 1.0)
- `tilt`: Relative tilt delta (-1.0 to 1.0)
- `zoom`: Relative zoom delta (-1.0 to 1.0)

#### 4. Get Presets

**ONVIF Method**: `PTZ.GetPresets`

```python
def get_presets(self):
    ptz_get_presets = self.get_complete_preset()
    
    presets = {}
    for i, preset in enumerate(ptz_get_presets):
        presets[preset.Name] = i
    return presets

def get_complete_preset(self):
    request = self.camera_ptz.create_type("GetPresets")
    request.ProfileToken = self.camera_media_profile.token
    ptz_get_presets = self.camera_ptz.GetPresets(request)
    return ptz_get_presets
```

**Response**: `{"Preset1": 0, "Preset2": 1, "ICU_Bed_View": 2}`

#### 5. Go To Preset

**ONVIF Method**: `PTZ.GotoPreset`

```python
@wait_for_movement_completion
def go_to_preset(self, preset_id: int):
    preset_list = self.get_complete_preset()
    request = self.camera_ptz.create_type("GotoPreset")
    request.ProfileToken = self.camera_media_profile.token
    
    for id, preset in enumerate(preset_list):
        if preset_id == id:
            request.PresetToken = preset.token
            self.camera_ptz.GotoPreset(request)
            return preset.Name
    
    logger.warning("Preset: %s not found!", preset_id)
    return None
```

**Input**: Preset index (0-based integer)
**Returns**: Preset name if found, None otherwise

#### 6. Set Preset

**ONVIF Method**: `PTZ.SetPreset`

```python
def set_preset(self, preset_name: str):
    presets = self.get_complete_preset()
    request = self.camera_ptz.create_type("SetPreset")
    request.ProfileToken = self.camera_media_profile.token
    request.PresetName = preset_name
    
    # Check if preset already exists
    for _, preset in enumerate(presets):
        if str(preset.Name) == preset_name:
            logger.warning("Preset ('%s') already exists!", preset_name)
            return None
    
    ptz_set_preset = self.camera_ptz.SetPreset(request)
    logger.debug("Preset ('%s') created!", preset_name)
    return ptz_set_preset
```

**Creates preset at current camera position**

#### 7. Get Snapshot URI

**ONVIF Method**: `Media.GetSnapshotUri`

```python
def get_snapshot_uri(self):
    request = self.camera_media.create_type("GetSnapshotUri")
    request.ProfileToken = self.camera_media_profile.token
    response = self.camera_media.GetSnapshotUri(request)
    return response.Uri
```

**Returns**: HTTP URL to JPEG snapshot
**Example**: `http://192.168.1.100/onvif/snapshot`

### Movement Completion Detection

**File**: `middleware/camera/utils.py`

**Problem**: ONVIF move commands return immediately, but physical movement takes time.

**Solution**: Decorator that polls camera status until movement completes.

```python
def wait_for_movement_completion(func):
    @functools.wraps(func)
    def wrapper_wait_for_movement_completion(self, *args, **kwargs):
        # Execute the movement command
        response = func(self, *args, **kwargs)
        
        # Poll until camera reports IDLE status
        while True:
            status = self.camera_ptz.GetStatus(
                {"ProfileToken": self.camera_media_profile.token}
            )
            
            # Check if both pan/tilt and zoom are idle
            if status.MoveStatus.PanTilt == "IDLE" and status.MoveStatus.Zoom == "IDLE":
                logger.info("Movement completed.")
                break
            
            # Sleep 500ms before checking again
            sleep(0.5)
        
        return response
    
    return wrapper_wait_for_movement_completion
```

**Applied to**:
- `absolute_move()`
- `relative_move()`
- `go_to_preset()`

**Polling Interval**: 500ms
**No Timeout**: Blocks until movement completes (potential improvement area)

---

## Authentication & Authorization

### JWT Token Structure

**Algorithm**: RS256 (RSA Signature with SHA-256)

**Key Generation**: `authlib` library
```python
from authlib.jose import JsonWebKey
import json, base64

JWKS = JsonWebKey.import_key_set(
    json.loads(base64.b64decode(env("JWKS_BASE64")))
)
```

**Token Claims**:

**Video Stream Token**:
```json
{
  "iat": 1234567890,        // Issued at (Unix timestamp)
  "exp": 1234568190,        // Expiry (iat + duration * 60)
  "stream": "camera-uuid",  // Stream identifier
  "ip": "192.168.1.100"     // Client IP address
}
```

**Vitals Stream Token**:
```json
{
  "iat": 1234567890,
  "exp": 1234568190,
  "asset_id": "uuid-string",
  "ip": "192.168.1.100"
}
```

**Gateway Token** (for CARE API communication):
```json
{
  "iat": 1234567890,
  "exp": 1234569090,
  "asset_id": "optional-uuid"
}
```

### Token Generation Function

**File**: `middleware/utils.py`

```python
def generate_jwt(claims=None, exp=60, jwks=None):
    if claims is None:
        claims = {}
    if jwks is None:
        jwks = settings.JWKS
    
    header = {"alg": "RS256"}
    time = int(datetime.now().timestamp())
    payload = {
        "iat": time,
        "exp": time + exp,
        **claims,
    }
    return jwt.encode(header, payload, jwks).decode("utf-8")
```

### Care API Authentication

**Middleware authenticates with external CARE platform**:

**File**: `common/authentication.py`

```python
class CareAuthentication(JWTAuthentication):
    auth_header_type = "Care_Bearer"
    
    def authenticate(self, request):
        header = self.get_header(request)
        raw_token = self.get_raw_token(header)
        
        # Validate token against CARE's JWKS endpoint
        open_id_url = f"{settings.CARE_API}/api/gateway_device/jwks.json/"
        validated_token = self.get_validated_token(open_id_url, raw_token)
        
        return self.get_user(validated_token), validated_token
    
    def get_public_key(self, url):
        # Cache JWKS for 5 minutes
        public_key_json = cache.get(f"jwk_response:{url}")
        if not public_key_json:
            res = requests.get(url)
            res.raise_for_status()
            public_key_json = res.json()
            cache.set(f"jwk_response:{url}", public_key_json, timeout=60 * 5)
        return public_key_json["keys"][0]
```

**Authorization Header**: `Authorization: Care_Bearer <jwt_token>`

### Public JWKS Endpoint

**Endpoint**: `GET /openid-configuration/`

**Purpose**: Allows external services to verify tokens issued by this gateway

**Returns**: JWKS (JSON Web Key Set)
```json
{
  "keys": [
    {
      "kty": "RSA",
      "use": "sig",
      "kid": "key-id",
      "n": "base64-encoded-modulus",
      "e": "AQAB"
    }
  ]
}
```

---

## WebSocket Communication

### Observation WebSocket

**Endpoint**: `ws://host:8001/observations/<ip_address>`

**Purpose**: Real-time streaming of device observations (vitals, monitor data)

**Implementation**: `middleware/consumers.py`

```python
class observations(WebsocketConsumer):
    def connect(self):
        ip = self.scope["url_route"]["kwargs"].get("ip_address", None)
        if ip:
            self.room_group_name = f"ip_{ip}"
            
            # Add to channel layer group
            async_to_sync(self.channel_layer.group_add)(
                self.room_group_name, 
                self.channel_name
            )
            
            self.accept()
    
    def disconnect(self, close_code):
        async_to_sync(self.channel_layer.group_discard)(
            self.room_group_name, 
            self.channel_name
        )
    
    def send_observation(self, event):
        message = event["message"]
        self.send(text_data=json.dumps(message))
```

**Message Flow**:
1. Device data arrives (HL7, ONVIF, etc.)
2. Backend publishes to channel layer: `channel_layer.group_send("ip_192.168.1.100", {...})`
3. WebSocket broadcasts to all connected clients for that IP

**Channel Layer**: Redis-backed (`channels_redis`)

### Logger WebSocket

**Endpoint**: `ws://host:8001/logger`

**Purpose**: System resource monitoring

```python
class LoggerConsumer(AsyncConsumer):
    async def websocket_connect(self, event):
        self.connected = True
        await self.send({"type": "websocket.accept"})
        
        while self.connected:
            await asyncio.sleep(2)
            uptime = (time.time() - psutil.boot_time()) * 1000
            state = {
                "type": "RESOURCE",
                "cpu": f"{psutil.cpu_percent(interval=1.0):.2f}",
                "memory": f"{psutil.virtual_memory().percent:.2f}",
                "uptime": uptime,
                "load": f"{psutil.getloadavg()[1]:.2f}",
            }
            await self.send({"type": "websocket.send", "text": json.dumps(state)})
```

**Update Interval**: 2 seconds

### WebSocket Authentication (Optional)

**Token via Subprotocol** (currently disabled):

```python
# Code exists but commented out
if b"sec-websocket-protocol" in headers:
    protocols = headers[b"sec-websocket-protocol"].decode().split(", ")
    for idx, protocol in enumerate(protocols):
        if protocol.startswith("Token"):
            token = protocols[idx + 1]
            self.is_token_verified(token=token)
```

**Current State**: WebSocket connections are NOT authenticated (TODO item)

---

## Redis State Management

### Key Patterns

#### 1. Camera Lock Keys
**Pattern**: `CAMERA_LOCK_KEY{ip_address}`
**Example**: `CAMERA_LOCK_KEY192.168.1.100`
**TTL**: 120 seconds (configurable via `CAMERA_LOCK_TIMEOUT`)
**Value**: `True` (presence indicates locked)

#### 2. Camera Status Keys
**Pattern**: `camera_statuses_{timestamp}`
**Example**: `camera_statuses_2024-01-15T10:30:00.000Z`
**TTL**: 1800 seconds (30 minutes)
**Value**: `{"192.168.1.100": "up", "192.168.1.101": "down"}`

#### 3. Monitor Status Keys
**Pattern**: `monitor_statuses_{timestamp}`
**Example**: `monitor_statuses_2024-01-15T10:30:00.000Z`
**TTL**: 1800 seconds
**Value**: Device status mappings

#### 4. Observation Keys
**Pattern**: `observations_{timestamp}`
**Example**: `observations_2024-01-15T10:30:00.000Z`
**TTL**: 1800 seconds
**Value**: Observation data batches

### Redis Manager

**File**: `middleware/redis_manager.py`

```python
class RedisManager:
    def push_to_redis(self, queue_name, item, expiry=60 * 30, curr_time=None):
        if not curr_time:
            curr_time = get_current_truncated_utc_z()
        
        redis_key = f"{queue_name}_{curr_time}"
        cache.set(redis_key, item, timeout=expiry)
    
    def get_redis_items(self, queue_name):
        search_pattern = f"{queue_name}*"
        matching_keys = cache.keys(search_pattern)
        
        # Sort keys by timestamp
        sorted_keys = sorted(
            matching_keys,
            key=lambda k: datetime.strptime(
                k.split(f"{queue_name}_")[1], 
                "%Y-%m-%dT%H:%M:%S.%fZ"
            ),
        )
        
        response_list = []
        for sorted_key in sorted_keys:
            statuses = cache.get(sorted_key)
            if statuses:
                timestamp = sorted_key.split(f"{queue_name}_")[1]
                response_list.append({"time": timestamp, "status": statuses})
        
        return response_list
```

**Timestamp Format**: ISO 8601 with milliseconds (e.g., `2024-01-15T10:30:00.000Z`)

**Truncation**: Seconds and microseconds set to 0 for minute-level bucketing

---

## Camera Locking Mechanism

### Purpose

**Prevents concurrent PTZ operations** that could conflict:
- Multiple users trying to move camera simultaneously
- Automated tasks interfering with manual control

### Implementation

**File**: `middleware/camera/utils.py`

```python
def lock_camera(ip: DeviceID):
    cache.set(
        f"{settings.CAMERA_LOCK_KEY}{ip}", 
        True, 
        settings.CAMERA_LOCK_TIMEOUT  # 120 seconds
    )

def unlock_camera(ip: DeviceID):
    cache.delete(f"{settings.CAMERA_LOCK_KEY}{ip}")

def is_camera_locked(ip: DeviceID):
    return cache.get(f"{settings.CAMERA_LOCK_KEY}{ip}")
```

### Lock Enforcement

**File**: `middleware/camera/views.py`

```python
class CameraViewSet(viewsets.ViewSet):
    def _return_if_camera_locked(self, device_id, raise_error=False):
        state = is_camera_locked(device_id)
        if state and raise_error:
            raise CameraLockedException
    
    @action(detail=False, methods=["post"], url_path="absoluteMove")
    def absolute_move(self, request):
        cam_request = CameraAssetMoveRequest.model_validate(request.data)
        
        # Check if camera is locked
        self._return_if_camera_locked(
            device_id=cam_request.hostname, 
            raise_error=True
        )
        
        cam = OnvifZeepCameraController(cam_request)
        cam.absolute_move(...)
```

**Locked Operations**:
- `absolute_move`
- `relative_move`
- `snapshot_at_location`

**NOT Locked** (read-only):
- `status`
- `presets`
- `go_to_preset`

### Lock Exception

**File**: `middleware/camera/exceptions.py`

```python
class CameraLockedException(APIException):
    status_code = status.HTTP_423_LOCKED  # HTTP 423
    default_detail = "Camera is Locked"
    default_code = "camera_error"
```

**HTTP Status**: 423 Locked (WebDAV extension)

### Lock Timeout

**Default**: 120 seconds
**Environment Variable**: `CAMERA_LOCK_TIMEOUT`

**Automatic Expiry**: Lock expires after timeout even if not explicitly unlocked

**Note**: Current implementation does NOT automatically lock/unlock. This appears to be infrastructure for future use or external lock management.

---

## API Endpoints Reference

### Camera Endpoints

**Base Path**: `/camera/`

#### GET /camera/status
**Description**: Get current camera PTZ position and status
**Query Parameters**:
- `hostname` (required): Camera IP address
- `port` (required): Camera port
- `username` (required): ONVIF username
- `password` (required): ONVIF password

**Response**:
```json
{
  "position": {
    "x": 0.5,
    "y": 0.2,
    "zoom": 0.1
  },
  "moveStatus": {
    "panTilt": "IDLE",
    "zoom": "IDLE"
  },
  "error": null
}
```

#### GET /camera/presets
**Description**: List all saved camera presets
**Query Parameters**: Same as `/status`

**Response**:
```json
{
  "Preset1": 0,
  "Preset2": 1,
  "ICU_View": 2
}
```

#### POST /camera/set_preset
**Description**: Save current position as preset
**Request Body**:
```json
{
  "hostname": "192.168.1.100",
  "port": 80,
  "username": "admin",
  "password": "password",
  "preset_name": "ICU_View"
}
```

#### POST /camera/gotoPreset
**Description**: Move camera to saved preset
**Request Body**:
```json
{
  "hostname": "192.168.1.100",
  "port": 80,
  "username": "admin",
  "password": "password",
  "preset": 2
}
```
**Returns**: Preset name (string) or 404 if not found

#### POST /camera/absoluteMove
**Description**: Move camera to absolute PTZ position
**Request Body**:
```json
{
  "hostname": "192.168.1.100",
  "port": 80,
  "username": "admin",
  "password": "password",
  "x": 0.5,
  "y": 0.2,
  "zoom": 0.1
}
```
**Blocks**: Until movement completes
**Lock Check**: Returns 423 if camera locked

#### POST /camera/relativeMove
**Description**: Move camera relative to current position
**Request Body**:
```json
{
  "hostname": "192.168.1.100",
  "port": 80,
  "username": "admin",
  "password": "password",
  "x": 0.1,
  "y": -0.1,
  "zoom": 0.05
}
```

#### POST /camera/snapshotAtLocation
**Description**: Move camera and get snapshot URI
**Request Body**: Same as `/relativeMove`
**Response**:
```json
{
  "status": "success",
  "uri": "http://192.168.1.100/onvif/snapshot"
}
```

#### GET /camera/cameras/status
**Description**: Get historical camera statuses from Redis
**Response**:
```json
[
  {
    "time": "2024-01-15T10:30:00.000Z",
    "status": {
      "192.168.1.100": "up",
      "192.168.1.101": "down"
    }
  }
]
```

### Stream Endpoints

**Base Path**: `/stream/`

#### POST /stream/getToken/videoFeed
**Description**: Generate JWT token for video stream access
**Authentication**: `Care_Bearer` token required
**Request Body**:
```json
{
  "stream": "camera-uuid",
  "ip": "192.168.1.50",
  "_duration": "10"
}
```
**Response**:
```json
{
  "token": "eyJhbGc..."
}
```

#### POST /stream/getToken/vitals
**Description**: Generate JWT token for vitals stream
**Authentication**: `Care_Bearer` token required
**Request Body**:
```json
{
  "asset_id": "asset-uuid",
  "ip": "192.168.1.50",
  "_duration": "10"
}
```

#### POST /stream/verifyToken
**Description**: Verify stream token validity
**Authentication**: None
**Request Body**:
```json
{
  "token": "eyJhbGc...",
  "ip": "192.168.1.50",
  "stream": "camera-uuid"
}
```
**Response**:
```json
{
  "status": "1"
}
```

### Health Endpoints

**Base Path**: `/health/`

#### GET /health/ping
**Description**: Simple health check
**Response**:
```json
{
  "pong": "2024-01-15T10:30:00.123456+05:30"
}
```

#### GET /health/status
**Description**: Check server and database health
**Response**:
```json
{
  "server": true,
  "database": true
}
```

#### GET /health/care/communication
**Description**: Test connection to CARE API
**Response**: Proxied from CARE API

#### GET /health/care/communication-asset
**Description**: Test CARE API connection as specific asset
**Query Parameters**:
- `ip` (optional): Asset IP address
- `ext_id` (optional): Asset external ID

### Observation Endpoints

**Base Path**: `/observations/`
(Implementation not shown in provided files, but referenced in URLs)

---

## Data Flow Diagrams

### Camera Control Flow

```
┌──────────────┐
│   Client     │
│  (Browser)   │
└──────┬───────┘
       │ POST /camera/absoluteMove
       │ {hostname, port, username, password, x, y, zoom}
       │
       ▼
┌──────────────────────────────────────┐
│   Nginx (Port 8001)                  │
│   - Rate Limiting: 20 req/s          │
└──────┬───────────────────────────────┘
       │
       ▼
┌──────────────────────────────────────┐
│   Django Middleware (Port 8090)      │
│   CameraViewSet.absolute_move()      │
└──────┬───────────────────────────────┘
       │
       ├─────────────────┐
       ▼                 ▼
┌─────────────┐   ┌─────────────────────┐
│   Redis     │   │  ONVIF Camera       │
│ Check Lock  │   │  (Port 80/554)      │
└─────┬───────┘   └──────┬──────────────┘
      │                  │
      │ Not Locked       │ 1. PTZ.AbsoluteMove
      │                  │ 2. Poll GetStatus every 500ms
      │                  │ 3. Wait for IDLE status
      │                  │
      ▼                  ▼
┌────────────────────────────────────────┐
│   Return Success Response              │
└────────────────────────────────────────┘
```

### Stream Access Flow

```
┌──────────────┐
│   Client     │
└──────┬───────┘
       │ 1. POST /stream/getToken/videoFeed
       │    Authorization: Care_Bearer <token>
       │    {stream: "uuid", ip: "x.x.x.x", _duration: "10"}
       ▼
┌──────────────────────────────────────┐
│   Django Middleware                  │
│   MiddlewareStreamViewSet            │
└──────┬───────────────────────────────┘
       │
       │ 2. Verify Care_Bearer token
       │    against CARE API JWKS
       ▼
┌──────────────────────────────────────┐
│   Generate JWT                       │
│   - Sign with local JWKS (RS256)    │
│   - Claims: {stream, ip}             │
│   - Expiry: duration * 60 seconds    │
└──────┬───────────────────────────────┘
       │
       │ 3. Return {token: "eyJhbGc..."}
       ▼
┌──────────────┐
│   Client     │
└──────┬───────┘
       │ 4. WS /stream/uuid?token=eyJhbGc...
       ▼
┌──────────────────────────────────────┐
│   Nginx                              │
│   Proxy to stream-server:8080        │
└──────┬───────────────────────────────┘
       │
       ▼
┌──────────────────────────────────────┐
│   RTSPtoWeb Service                  │
│   - Verify JWT (calls /verifyToken)  │
│   - Lookup RTSP URL from config      │
│   - Convert RTSP → WebRTC/HLS        │
│   - Stream to client                 │
└──────────────────────────────────────┘
```

### Camera Status Monitoring Flow

```
┌─────────────────────────────────────┐
│   Celery Beat Scheduler             │
│   Every N minutes                    │
└──────┬──────────────────────────────┘
       │ Trigger store_camera_statuses
       ▼
┌─────────────────────────────────────┐
│   Celery Worker                     │
│   store_camera_statuses task        │
└──────┬──────────────────────────────┘
       │
       │ 1. Query PostgreSQL for ONVIF assets
       │    Asset.objects.filter(type=ONVIF, deleted=False)
       ▼
┌─────────────────────────────────────┐
│   For each camera:                  │
│   - Create OnvifZeepCameraController│
│   - Call get_status()               │
│   - Check error field               │
└──────┬──────────────────────────────┘
       │
       │ 2. Build status dict
       │    {ip: "up" | "down"}
       ▼
┌─────────────────────────────────────┐
│   Redis Manager                     │
│   push_to_redis(                    │
│     "camera_statuses",              │
│     status_dict,                    │
│     expiry=1800                     │
│   )                                 │
└──────┬──────────────────────────────┘
       │
       │ Key: camera_statuses_2024-01-15T10:30:00.000Z
       │ TTL: 30 minutes
       ▼
┌─────────────────────────────────────┐
│   Redis Cache                       │
└─────────────────────────────────────┘
```

---

## Database Models

### Asset Model

**File**: `middleware/models.py`

**Table**: `middleware_asset`

```python
class Asset(models.Model):
    id = models.UUIDField(primary_key=True, default=uuid.uuid4, editable=False)
    name = models.CharField(max_length=255)
    type = models.CharField(
        max_length=20,
        choices=AssetClasses.as_choices(),
        default=AssetClasses.HL7MONITOR.value,
    )
    description = models.TextField()
    ip_address = models.GenericIPAddressField()
    created_at = models.DateTimeField(default=timezone.now)
    updated_at = models.DateTimeField(default=timezone.now)
    deleted = models.BooleanField(default=False)
    access_key = models.CharField(max_length=255, null=True, blank=True)
    username = models.CharField(max_length=255, null=True, blank=True)
    password = models.CharField(max_length=255, null=True, blank=True)
    port = models.IntegerField(default=80, null=True, blank=True)
```

**Indexes**:
- `(id, ip_address)` - Composite index for fast lookups

**Asset Types**:
- `ONVIF` - ONVIF-compliant cameras
- `HL7MONITOR` - HL7-enabled vital signs monitors
- `VENTILATOR` - Ventilator devices

**Camera-Specific Fields**:
- `ip_address`: Camera hostname/IP
- `port`: ONVIF service port (usually 80)
- `username`: ONVIF authentication username
- `password`: ONVIF authentication password (stored in plaintext - security concern)

### DailyRound Model

**Table**: `middleware_dailyround`

```python
class DailyRound(models.Model):
    asset_external_id = models.UUIDField()
    status = models.CharField(max_length=255)
    data = models.TextField()
    response = models.TextField()
    time = models.DateTimeField(default=timezone.now)
    asset = models.ForeignKey(
        Asset, 
        on_delete=models.CASCADE, 
        related_name="daily_rounds"
    )
```

**Indexes**:
- `asset_external_id` - For filtering by external ID

**Purpose**: Log observation submissions and responses from CARE API

---
