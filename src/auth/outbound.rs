use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// The gateway's own RSA keypair used for signing outbound JWTs
/// and exposing the public key via /openid-configuration/.
pub struct OwnKeypair {
    pub private_key: RsaPrivateKey,
    pub encoding_key: EncodingKey,
    pub public_jwk: Value,
}

/// Standard JWT claims for gateway-issued tokens.
#[derive(Debug, Serialize, Deserialize)]
pub struct GatewayClaims {
    pub iat: u64,
    pub exp: u64,
    #[serde(flatten)]
    pub extra: Value,
}

impl OwnKeypair {
    /// Load or generate the gateway's RSA keypair.
    ///
    /// Priority:
    /// 1. If `jwks_base64_override` is set, decode from base64 JWK.
    /// 2. If `{state_dir}/jwks.json` exists, load from file.
    /// 3. Otherwise generate a new RSA-2048 keypair, save to file.
    pub fn load_or_generate(
        state_dir: &str,
        jwks_base64_override: Option<&str>,
    ) -> anyhow::Result<Self> {
        // Option 1: from base64-encoded JWKS env var
        if let Some(b64) = jwks_base64_override {
            tracing::info!("Loading keypair from JWKS_BASE64 environment variable");
            let json_bytes = BASE64.decode(b64)?;
            let jwks: Value = serde_json::from_slice(&json_bytes)?;
            return Self::from_jwk_value(&jwks);
        }

        let key_path = Path::new(state_dir).join("jwks.json");

        // Option 2: load from file
        if key_path.exists() {
            tracing::info!("Loading keypair from {}", key_path.display());
            let data = fs::read_to_string(&key_path)?;
            let jwks: Value = serde_json::from_str(&data)?;
            return Self::from_jwk_value(&jwks);
        }

        // Option 3: generate new keypair
        tracing::info!("Generating new RSA-2048 keypair");
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048)?;

        let keypair = Self::from_rsa_private_key(private_key)?;

        // Persist full private key to state_dir so it can be reloaded on restart
        fs::create_dir_all(state_dir)?;
        let private_jwk = Self::private_key_to_jwk(&keypair.private_key)?;
        let jwks_json = json!({
            "keys": [private_jwk]
        });
        fs::write(&key_path, serde_json::to_string_pretty(&jwks_json)?)?;
        tracing::info!("Saved keypair to {}", key_path.display());

        Ok(keypair)
    }

    /// Build OwnKeypair from a JWK keyset value (with "keys" array).
    fn from_jwk_value(jwks: &Value) -> anyhow::Result<Self> {
        // The JWKS may be a keyset with "keys" array or a single key
        let key_value = if let Some(keys) = jwks.get("keys").and_then(|k| k.as_array()) {
            keys.first()
                .ok_or_else(|| anyhow::anyhow!("empty JWKS keyset"))?
                .clone()
        } else {
            jwks.clone()
        };

        // Extract RSA components from JWK and reconstruct the private key.
        // We need n, e, d, p, q at minimum.
        let n = jwk_param_to_biguint(&key_value, "n")?;
        let e = jwk_param_to_biguint(&key_value, "e")?;
        let d = jwk_param_to_biguint(&key_value, "d")?;
        let primes = {
            let mut v = Vec::new();
            if let Ok(p) = jwk_param_to_biguint(&key_value, "p") {
                v.push(p);
            }
            if let Ok(q) = jwk_param_to_biguint(&key_value, "q") {
                v.push(q);
            }
            v
        };

        let private_key = rsa::RsaPrivateKey::from_components(n, e, d, primes)?;

        Self::from_rsa_private_key(private_key)
    }

    /// Build OwnKeypair from an rsa::RsaPrivateKey.
    fn from_rsa_private_key(private_key: RsaPrivateKey) -> anyhow::Result<Self> {
        // Create encoding key from PKCS8 PEM
        let pem = private_key.to_pkcs8_pem(LineEnding::LF)?;
        let encoding_key = EncodingKey::from_rsa_pem(pem.as_bytes())?;

        // Build public JWK
        let public_key = private_key.to_public_key();
        let _pub_pem = public_key.to_public_key_pem(LineEnding::LF)?;

        // Build a JWK representation of the public key
        let n_bytes = public_key.n().to_bytes_be();
        let e_bytes = public_key.e().to_bytes_be();
        let n_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&n_bytes);
        let e_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&e_bytes);

        let public_jwk = json!({
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "n": n_b64,
            "e": e_b64,
        });

        Ok(Self {
            private_key,
            encoding_key,
            public_jwk,
        })
    }

    /// Sign a JWT with the gateway's private key.
    pub fn sign_jwt(&self, extra_claims: Value, exp_secs: u64) -> anyhow::Result<String> {
        let now = chrono::Utc::now().timestamp() as u64;
        let claims = GatewayClaims {
            iat: now,
            exp: now + exp_secs,
            extra: extra_claims,
        };
        let header = Header::new(Algorithm::RS256);
        Ok(encode(&header, &claims, &self.encoding_key)?)
    }

    /// Validate a JWT against the gateway's own public key.
    pub fn verify_jwt(&self, token: &str) -> Result<GatewayClaims, jsonwebtoken::errors::Error> {
        let pub_key = self.private_key.to_public_key();
        let pub_pem =
            pub_key.to_public_key_pem(LineEnding::LF).map_err(|_| {
                jsonwebtoken::errors::Error::from(jsonwebtoken::errors::ErrorKind::InvalidKeyFormat)
            })?;
        let decoding_key = jsonwebtoken::DecodingKey::from_rsa_pem(pub_pem.as_bytes())?;
        let mut validation = jsonwebtoken::Validation::new(Algorithm::RS256);
        validation.validate_exp = true;
        validation.set_required_spec_claims(&["exp"]);
        let data = jsonwebtoken::decode::<GatewayClaims>(token, &decoding_key, &validation)?;
        Ok(data.claims)
    }

    /// Get the public JWKS response body for GET /openid-configuration/.
    pub fn public_jwks(&self) -> Value {
        json!({
            "keys": [self.public_jwk.clone()]
        })
    }

    /// Serialize the full RSA private key as a JWK value (includes d, p, q, dp, dq, qi).
    fn private_key_to_jwk(key: &RsaPrivateKey) -> anyhow::Result<Value> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use rsa::traits::PrivateKeyParts;

        let public_key = key.to_public_key();
        let n_b64 = URL_SAFE_NO_PAD.encode(public_key.n().to_bytes_be());
        let e_b64 = URL_SAFE_NO_PAD.encode(public_key.e().to_bytes_be());
        let d_b64 = URL_SAFE_NO_PAD.encode(key.d().to_bytes_be());

        let primes = key.primes();
        let mut jwk = json!({
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "n": n_b64,
            "e": e_b64,
            "d": d_b64,
        });

        if primes.len() >= 2 {
            jwk["p"] = json!(URL_SAFE_NO_PAD.encode(primes[0].to_bytes_be()));
            jwk["q"] = json!(URL_SAFE_NO_PAD.encode(primes[1].to_bytes_be()));

            // Include CRT components if available (dp, dq, qi)
            if let Some(dp) = key.dp() {
                jwk["dp"] = json!(URL_SAFE_NO_PAD.encode(dp.to_bytes_be()));
            }
            if let Some(dq) = key.dq() {
                jwk["dq"] = json!(URL_SAFE_NO_PAD.encode(dq.to_bytes_be()));
            }
            if let Some(qi) = key.qinv() {
                let qi_bytes = qi.to_biguint()
                    .map(|b| b.to_bytes_be())
                    .unwrap_or_default();
                jwk["qi"] = json!(URL_SAFE_NO_PAD.encode(qi_bytes));
            }
        }

        Ok(jwk)
    }
}

/// Extract a base64url-encoded big integer from a JWK field.
fn jwk_param_to_biguint(jwk: &Value, field: &str) -> anyhow::Result<rsa::BigUint> {
    let b64 = jwk
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing JWK field: {field}"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64)?;
    Ok(rsa::BigUint::from_bytes_be(&bytes))
}
