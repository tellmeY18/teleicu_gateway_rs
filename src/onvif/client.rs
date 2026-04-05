use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;
use std::time::Duration;
use tokio::time::{sleep, timeout};

use super::soap;
use crate::error::AppError;

/// A camera profile returned by GetProfiles.
#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub token: String,
    pub name: String,
}

/// PTZ position.
#[derive(Debug, Clone, Serialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

/// PTZ move status.
#[derive(Debug, Clone, Serialize)]
pub struct MoveStatus {
    #[serde(rename = "panTilt")]
    pub pan_tilt: String,
    pub zoom: String,
}

/// Full PTZ status returned by GetStatus.
#[derive(Debug, Clone, Serialize)]
pub struct PtzStatus {
    pub position: Position,
    #[serde(rename = "moveStatus")]
    pub move_status: MoveStatus,
    pub error: Option<String>,
}

/// A camera preset.
#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub token: String,
    pub name: String,
}

/// Stateless ONVIF client — one per request, credentials come from the caller.
pub struct OnvifClient {
    http: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
}

impl OnvifClient {
    pub fn new(http: reqwest::Client, hostname: &str, port: u16, username: &str, password: &str) -> Self {
        Self {
            http,
            base_url: format!("http://{}:{}", hostname, port),
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    /// Send a SOAP request to the given service path and return the response body as string.
    async fn soap_request(&self, service_path: &str, body_xml: &str) -> Result<String, AppError> {
        let envelope = soap::soap_envelope(&self.username, &self.password, body_xml);
        let url = format!("{}{}", self.base_url, service_path);

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/soap+xml; charset=utf-8")
            .body(envelope)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    AppError::InvalidCameraCredentials
                } else {
                    AppError::Onvif(format!("request failed: {e}"))
                }
            })?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::Onvif(format!("failed to read response: {e}")))?;

        if status == reqwest::StatusCode::UNAUTHORIZED
            || text.contains("NotAuthorized")
            || text.contains("sender not Authorized")
        {
            return Err(AppError::InvalidCameraCredentials);
        }

        if !status.is_success() {
            return Err(AppError::Onvif(format!("SOAP fault (HTTP {status}): {text}")));
        }

        Ok(text)
    }

    /// Get camera profiles.
    pub async fn get_profiles(&self) -> Result<Vec<Profile>, AppError> {
        let body = soap::get_profiles_body();
        let resp = self.soap_request("/onvif/media_service", &body).await?;
        parse_profiles(&resp)
    }

    /// Get PTZ status for a profile.
    pub async fn get_status(&self, profile_token: &str) -> Result<PtzStatus, AppError> {
        let body = soap::get_status_body(profile_token);
        let resp = self.soap_request("/onvif/ptz_service", &body).await?;
        parse_ptz_status(&resp)
    }

    /// Get presets for a profile.
    pub async fn get_presets(&self, profile_token: &str) -> Result<Vec<Preset>, AppError> {
        let body = soap::get_presets_body(profile_token);
        let resp = self.soap_request("/onvif/ptz_service", &body).await?;
        parse_presets(&resp)
    }

    /// Go to a preset by token.
    pub async fn goto_preset(&self, profile_token: &str, preset_token: &str) -> Result<String, AppError> {
        let body = soap::goto_preset_body(profile_token, preset_token);
        self.soap_request("/onvif/ptz_service", &body).await
    }

    /// Set (create/update) a preset with the given name.
    /// Returns an error if a preset with that name already exists.
    pub async fn set_preset(&self, profile_token: &str, preset_name: &str) -> Result<(), AppError> {
        // Check for name collision
        let existing = self.get_presets(profile_token).await?;
        if existing.iter().any(|p| p.name == preset_name) {
            return Err(AppError::Onvif(format!(
                "preset with name '{preset_name}' already exists"
            )));
        }

        let body = soap::set_preset_body(profile_token, preset_name);
        self.soap_request("/onvif/ptz_service", &body).await?;
        Ok(())
    }

    /// Absolute PTZ move.
    pub async fn absolute_move(&self, profile_token: &str, pan: f32, tilt: f32, zoom: f32) -> Result<(), AppError> {
        let body = soap::absolute_move_body(profile_token, pan, tilt, zoom);
        self.soap_request("/onvif/ptz_service", &body).await?;
        Ok(())
    }

    /// Relative PTZ move.
    pub async fn relative_move(&self, profile_token: &str, pan: f32, tilt: f32, zoom: f32) -> Result<(), AppError> {
        let body = soap::relative_move_body(profile_token, pan, tilt, zoom);
        self.soap_request("/onvif/ptz_service", &body).await?;
        Ok(())
    }

    /// Get snapshot URI for a profile.
    pub async fn get_snapshot_uri(&self, profile_token: &str) -> Result<String, AppError> {
        let body = soap::get_snapshot_uri_body(profile_token);
        let resp = self.soap_request("/onvif/media_service", &body).await?;
        parse_snapshot_uri(&resp)
    }

    /// Poll until PTZ movement is idle, with a timeout.
    pub async fn wait_for_idle(&self, profile_token: &str, timeout_secs: u64) -> Result<(), AppError> {
        let result = timeout(Duration::from_secs(timeout_secs), async {
            loop {
                let status = self.get_status(profile_token).await?;
                let pan_idle = status.move_status.pan_tilt.to_uppercase() == "IDLE";
                let zoom_idle = status.move_status.zoom.to_uppercase() == "IDLE";
                if pan_idle && zoom_idle {
                    return Ok::<(), AppError>(());
                }
                sleep(Duration::from_millis(500)).await;
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_) => Err(AppError::Onvif("movement timed out".into())),
        }
    }
}

// ---------- XML parsing helpers ----------

/// Extract text content from the first occurrence of a tag (by local name).
fn find_tag_text(xml: &str, local_name: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut in_target = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == local_name {
                    in_target = true;
                }
            }
            Ok(Event::Text(e)) if in_target => {
                return Some(e.unescape().unwrap_or_default().to_string());
            }
            Ok(Event::End(_)) if in_target => {
                in_target = false;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// Extract an attribute value from the first occurrence of a tag (by local name).
fn find_tag_attr(xml: &str, local_name: &str, attr_name: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == local_name {
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        if key == attr_name {
                            return Some(String::from_utf8_lossy(&attr.value).to_string());
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// Parse GetProfiles response.
fn parse_profiles(xml: &str) -> Result<Vec<Profile>, AppError> {
    let mut profiles = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut current_token = None;
    let mut current_name = None;
    let mut in_profiles_response = false;
    let mut in_name = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if local == "Profiles" {
                    in_profiles_response = true;
                    // Get token attribute
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        if key == "token" {
                            current_token = Some(String::from_utf8_lossy(&attr.value).to_string());
                        }
                    }
                } else if local == "Name" && in_profiles_response {
                    in_name = true;
                }
            }
            Ok(Event::Text(e)) if in_name => {
                current_name = Some(e.unescape().unwrap_or_default().to_string());
            }
            Ok(Event::End(e)) => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if local == "Name" {
                    in_name = false;
                }
                if local == "Profiles" {
                    in_profiles_response = false;
                    if let (Some(token), Some(name)) = (current_token.take(), current_name.take()) {
                        profiles.push(Profile { token, name });
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(AppError::Onvif(format!("XML parse error: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    Ok(profiles)
}

/// Parse GetStatus response into PtzStatus.
fn parse_ptz_status(xml: &str) -> Result<PtzStatus, AppError> {
    let pan_tilt_x = find_tag_attr(xml, "PanTilt", "x")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);
    let pan_tilt_y = find_tag_attr(xml, "PanTilt", "y")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);
    let zoom_val = find_tag_attr(xml, "Zoom", "x")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);

    let pan_tilt_status = find_tag_text(xml, "PanTilt").unwrap_or_else(|| "IDLE".into());
    let zoom_status = find_tag_text(xml, "Zoom").unwrap_or_else(|| "IDLE".into());
    let error = find_tag_text(xml, "Error").or_else(|| find_tag_text(xml, "error"));

    // Normalize error: treat "noerror" / "NO error" as None
    let error = error.and_then(|e| {
        let normalized = e.to_lowercase().replace(' ', "");
        if normalized == "noerror" {
            None
        } else {
            Some(e)
        }
    });

    // Disambiguate: PanTilt appears in both Position and MoveStatus.
    // MoveStatus PanTilt contains text like "IDLE"/"MOVING", Position PanTilt has x/y attributes.
    // We already extracted position from attributes, and status from text content.

    Ok(PtzStatus {
        position: Position {
            x: pan_tilt_x,
            y: pan_tilt_y,
            zoom: zoom_val,
        },
        move_status: MoveStatus {
            pan_tilt: pan_tilt_status,
            zoom: zoom_status,
        },
        error,
    })
}

/// Parse GetPresets response.
fn parse_presets(xml: &str) -> Result<Vec<Preset>, AppError> {
    let mut presets = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut current_token = None;
    let mut current_name = None;
    let mut in_preset = false;
    let mut in_name = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if local == "Preset" {
                    in_preset = true;
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        if key == "token" {
                            current_token = Some(String::from_utf8_lossy(&attr.value).to_string());
                        }
                    }
                } else if local == "Name" && in_preset {
                    in_name = true;
                }
            }
            Ok(Event::Text(e)) if in_name => {
                current_name = Some(e.unescape().unwrap_or_default().to_string());
            }
            Ok(Event::End(e)) => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if local == "Name" {
                    in_name = false;
                }
                if local == "Preset" {
                    in_preset = false;
                    if let (Some(token), Some(name)) = (current_token.take(), current_name.take()) {
                        presets.push(Preset { token, name });
                    } else if let Some(token) = current_token.take() {
                        presets.push(Preset {
                            token,
                            name: String::new(),
                        });
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(AppError::Onvif(format!("XML parse error: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    Ok(presets)
}

/// Parse GetSnapshotUri response.
fn parse_snapshot_uri(xml: &str) -> Result<String, AppError> {
    find_tag_text(xml, "Uri")
        .or_else(|| find_tag_text(xml, "uri"))
        .ok_or_else(|| AppError::Onvif("no URI in GetSnapshotUri response".into()))
}
