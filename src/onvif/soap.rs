use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::Utc;
use sha1::{Digest, Sha1};

/// SOAP XML namespace constants.
pub const NS_ENVELOPE: &str = "http://www.w3.org/2003/05/soap-envelope";
pub const NS_WSSE: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
pub const NS_WSU: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
pub const NS_PTZ: &str = "http://www.onvif.org/ver20/ptz/wsdl";
pub const NS_MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";
pub const NS_DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
pub const NS_SCHEMA: &str = "http://www.onvif.org/ver10/schema";

/// Build a WS-Security header XML block for ONVIF authentication.
///
/// password_digest = base64(SHA-1(nonce_raw ++ created_utf8 ++ password_utf8))
pub fn ws_security_header(username: &str, password: &str) -> String {
    let mut nonce_raw = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce_raw);
    let nonce_b64 = BASE64.encode(nonce_raw);
    let created = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // SHA-1(nonce_raw + created + password)
    let mut hasher = Sha1::new();
    hasher.update(&nonce_raw);
    hasher.update(created.as_bytes());
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();
    let digest_b64 = BASE64.encode(digest);

    format!(
        r#"<wsse:Security s:mustUnderstand="true" xmlns:wsse="{NS_WSSE}" xmlns:wsu="{NS_WSU}">
  <wsse:UsernameToken>
    <wsse:Username>{username}</wsse:Username>
    <wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{digest_b64}</wsse:Password>
    <wsse:Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{nonce_b64}</wsse:Nonce>
    <wsu:Created>{created}</wsu:Created>
  </wsse:UsernameToken>
</wsse:Security>"#
    )
}

/// Build a complete SOAP envelope with WS-Security header and the given body XML.
pub fn soap_envelope(username: &str, password: &str, body: &str) -> String {
    let security = ws_security_header(username, password);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="{NS_ENVELOPE}">
  <s:Header>
    {security}
  </s:Header>
  <s:Body>
    {body}
  </s:Body>
</s:Envelope>"#
    )
}

/// Build the SOAP body for GetProfiles (Media service).
pub fn get_profiles_body() -> String {
    format!(r#"<GetProfiles xmlns="{NS_MEDIA}"/>"#)
}

/// Build the SOAP body for GetStatus (PTZ service).
pub fn get_status_body(profile_token: &str) -> String {
    format!(
        r#"<GetStatus xmlns="{NS_PTZ}">
  <ProfileToken>{profile_token}</ProfileToken>
</GetStatus>"#
    )
}

/// Build the SOAP body for GetPresets (PTZ service).
pub fn get_presets_body(profile_token: &str) -> String {
    format!(
        r#"<GetPresets xmlns="{NS_PTZ}">
  <ProfileToken>{profile_token}</ProfileToken>
</GetPresets>"#
    )
}

/// Build the SOAP body for GotoPreset (PTZ service).
pub fn goto_preset_body(profile_token: &str, preset_token: &str) -> String {
    format!(
        r#"<GotoPreset xmlns="{NS_PTZ}">
  <ProfileToken>{profile_token}</ProfileToken>
  <PresetToken>{preset_token}</PresetToken>
</GotoPreset>"#
    )
}

/// Build the SOAP body for SetPreset (PTZ service).
pub fn set_preset_body(profile_token: &str, preset_name: &str) -> String {
    format!(
        r#"<SetPreset xmlns="{NS_PTZ}">
  <ProfileToken>{profile_token}</ProfileToken>
  <PresetName>{preset_name}</PresetName>
</SetPreset>"#
    )
}

/// Build the SOAP body for AbsoluteMove (PTZ service).
pub fn absolute_move_body(profile_token: &str, pan: f32, tilt: f32, zoom: f32) -> String {
    format!(
        r#"<AbsoluteMove xmlns="{NS_PTZ}">
  <ProfileToken>{profile_token}</ProfileToken>
  <Position>
    <PanTilt x="{pan}" y="{tilt}" xmlns="{NS_SCHEMA}"/>
    <Zoom x="{zoom}" xmlns="{NS_SCHEMA}"/>
  </Position>
</AbsoluteMove>"#
    )
}

/// Build the SOAP body for RelativeMove (PTZ service).
pub fn relative_move_body(profile_token: &str, pan: f32, tilt: f32, zoom: f32) -> String {
    format!(
        r#"<RelativeMove xmlns="{NS_PTZ}">
  <ProfileToken>{profile_token}</ProfileToken>
  <Translation>
    <PanTilt x="{pan}" y="{tilt}" xmlns="{NS_SCHEMA}"/>
    <Zoom x="{zoom}" xmlns="{NS_SCHEMA}"/>
  </Translation>
</RelativeMove>"#
    )
}

/// Build the SOAP body for GetSnapshotUri (Media service).
pub fn get_snapshot_uri_body(profile_token: &str) -> String {
    format!(
        r#"<GetSnapshotUri xmlns="{NS_MEDIA}">
  <ProfileToken>{profile_token}</ProfileToken>
</GetSnapshotUri>"#
    )
}
