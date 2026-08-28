// Cookie import/export for ShardX profiles (Chromium Cookies sqlite v24).
// v10 blob = "v10" + cipher; plaintext = SHA256(host) + value.
//   macOS: AES-128-CBC,  key = PBKDF2(mock_password, saltysalt, 1003)
//   Linux: AES-128-CBC,  key = PBKDF2(peanuts,       saltysalt, 1)
//   Win:   AES-256-GCM,  key = DPAPI-unwrapped Local State os_crypt.encrypted_key

use crate::profile;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

// ---- AES-128-CBC cipher (macOS + Linux) ----
#[cfg(not(target_os = "windows"))]
use aes::Aes128;
#[cfg(not(target_os = "windows"))]
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
#[cfg(not(target_os = "windows"))]
type Aes128CbcDec = cbc::Decryptor<Aes128>;
#[cfg(not(target_os = "windows"))]
type Aes128CbcEnc = cbc::Encryptor<Aes128>;
#[cfg(not(target_os = "windows"))]
const IV: [u8; 16] = [0x20; 16];

/// Tool-friendly cookie shape (httpOnly / sameSite camelCase aliases accepted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub domain: String,
    pub name: String,
    pub value: String,
    #[serde(default = "default_path")]
    pub path: String,
    /// Unix seconds; None = session cookie.
    #[serde(default)]
    pub expires: Option<f64>,
    #[serde(default)]
    pub secure: bool,
    #[serde(default, alias = "httpOnly")]
    pub http_only: bool,
    /// "Strict" | "Lax" | "None" | "unspecified" (case-insensitive).
    #[serde(default, alias = "sameSite")]
    pub same_site: Option<String>,
}

fn default_path() -> String {
    "/".to_string()
}

// Chromium time = µs since 1601-01-01.
const WIN_EPOCH_DELTA_SECS: i64 = 11_644_473_600;

fn chromium_to_unix_secs(micros: i64) -> f64 {
    micros as f64 / 1_000_000.0 - WIN_EPOCH_DELTA_SECS as f64
}
fn unix_to_chromium(secs: f64) -> i64 {
    ((secs + WIN_EPOCH_DELTA_SECS as f64) * 1_000_000.0) as i64
}
fn now_chromium() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    unix_to_chromium(now)
}

// ---- OSCrypt key + cipher (per OS) ----

/// Resolved OSCrypt key; cipher chosen per-OS at compile time.
struct Crypt {
    key: Vec<u8>,
}

impl Crypt {
    /// POSIX: fixed derivation; Windows: read/mint os_crypt.encrypted_key.
    fn open(udd: &Path) -> Result<Self> {
        Ok(Self {
            key: os_crypt_key(udd)?,
        })
    }

    fn decrypt(&self, encrypted: &[u8], plain: &str) -> String {
        // Legacy rows: value in `value` column, no v10 blob.
        if encrypted.len() < 3 || &encrypted[..3] != b"v10" {
            return plain.to_string();
        }
        match cipher_decrypt(&self.key, &encrypted[3..]) {
            Some(pt) => String::from_utf8_lossy(&strip_host_prefix(pt)).into_owned(),
            None => String::new(),
        }
    }

    fn encrypt(&self, host: &str, value: &str) -> Vec<u8> {
        // 32-byte SHA256(host) prefix per Chromium ≥130.
        let mut plaintext = Sha256::digest(host.as_bytes()).to_vec();
        plaintext.extend_from_slice(value.as_bytes());
        let body = cipher_encrypt(&self.key, &plaintext);
        let mut out = Vec::with_capacity(3 + body.len());
        out.extend_from_slice(b"v10");
        out.extend_from_slice(&body);
        out
    }
}

/// Strip the 32-byte SHA256(host) domain prefix.
fn strip_host_prefix(mut pt: Vec<u8>) -> Vec<u8> {
    if pt.len() >= 32 {
        pt.drain(0..32);
    }
    pt
}

// ---- macOS: mock_password ----
#[cfg(target_os = "macos")]
fn os_crypt_key(_udd: &Path) -> Result<Vec<u8>> {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(b"mock_password", b"saltysalt", 1003, &mut key);
    Ok(key.to_vec())
}

// ---- Linux: peanuts ----
#[cfg(target_os = "linux")]
fn os_crypt_key(_udd: &Path) -> Result<Vec<u8>> {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(b"peanuts", b"saltysalt", 1, &mut key);
    Ok(key.to_vec())
}

// ---- POSIX CBC ----
#[cfg(not(target_os = "windows"))]
fn cipher_decrypt(key: &[u8], body: &[u8]) -> Option<Vec<u8>> {
    let dec = Aes128CbcDec::new_from_slices(key, &IV).ok()?;
    dec.decrypt_padded_vec_mut::<Pkcs7>(body).ok()
}
#[cfg(not(target_os = "windows"))]
fn cipher_encrypt(key: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let enc = Aes128CbcEnc::new_from_slices(key, &IV).expect("16-byte key/iv");
    enc.encrypt_padded_vec_mut::<Pkcs7>(plaintext)
}

// ---- Windows: DPAPI key + AES-256-GCM ----
#[cfg(target_os = "windows")]
fn os_crypt_key(udd: &Path) -> Result<Vec<u8>> {
    win::os_crypt_key(udd)
}
#[cfg(target_os = "windows")]
fn cipher_decrypt(key: &[u8], body: &[u8]) -> Option<Vec<u8>> {
    win::gcm_decrypt(key, body)
}
#[cfg(target_os = "windows")]
fn cipher_encrypt(key: &[u8], plaintext: &[u8]) -> Vec<u8> {
    win::gcm_encrypt(key, plaintext)
}

#[cfg(target_os = "windows")]
mod win {
    use aes_gcm::{
        aead::{Aead, KeyInit},
        Aes256Gcm, Key, Nonce,
    };
    use anyhow::{anyhow, Context, Result};
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::path::Path;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    const DPAPI_TAG: &[u8] = b"DPAPI";

    fn rand_bytes<const N: usize>() -> [u8; N] {
        let mut b = [0u8; N];
        getrandom::getrandom(&mut b).expect("getrandom");
        b
    }

    // v10 body = nonce(12) || ciphertext || tag(16)
    pub fn gcm_decrypt(key: &[u8], body: &[u8]) -> Option<Vec<u8>> {
        if key.len() != 32 || body.len() < 12 + 16 {
            return None;
        }
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        let (nonce, ct) = body.split_at(12);
        cipher.decrypt(Nonce::from_slice(nonce), ct).ok()
    }

    pub fn gcm_encrypt(key: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        let nonce = rand_bytes::<12>();
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .expect("gcm encrypt");
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    // ---- DPAPI wrap/unwrap ----
    unsafe fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>> {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: input.len() as u32,
            pbData: input.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = if protect {
            CryptProtectData(
                &in_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out_blob,
            )
        } else {
            CryptUnprotectData(
                &in_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out_blob,
            )
        };
        if ok == 0 {
            return Err(anyhow!(
                "DPAPI {} failed",
                if protect { "protect" } else { "unprotect" }
            ));
        }
        let out = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
        LocalFree(out_blob.pbData as _);
        Ok(out)
    }

    /// Read os_crypt key, or mint+persist one for never-launched profiles.
    pub fn os_crypt_key(udd: &Path) -> Result<Vec<u8>> {
        let ls_path = udd.join("Local State");
        if let Some(key) = read_key(&ls_path)? {
            return Ok(key);
        }
        let key = rand_bytes::<32>().to_vec();
        write_key(&ls_path, &key)?;
        Ok(key)
    }

    fn read_key(ls_path: &Path) -> Result<Option<Vec<u8>>> {
        if !ls_path.exists() {
            return Ok(None);
        }
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ls_path).context("read Local State")?)
                .context("parse Local State")?;
        let enc = match json
            .get("os_crypt")
            .and_then(|o| o.get("encrypted_key"))
            .and_then(|k| k.as_str())
        {
            Some(s) => s,
            None => return Ok(None),
        };
        let blob = STANDARD.decode(enc).context("base64 encrypted_key")?;
        if blob.len() <= DPAPI_TAG.len() || &blob[..DPAPI_TAG.len()] != DPAPI_TAG {
            return Err(anyhow!("encrypted_key missing DPAPI tag"));
        }
        Ok(Some(unsafe { dpapi(&blob[DPAPI_TAG.len()..], false)? }))
    }

    fn write_key(ls_path: &Path, key: &[u8]) -> Result<()> {
        let wrapped = unsafe { dpapi(key, true)? };
        let mut tagged = DPAPI_TAG.to_vec();
        tagged.extend_from_slice(&wrapped);
        let b64 = STANDARD.encode(&tagged);

        // Merge into existing Local State or create minimal one.
        let mut json: serde_json::Value = if ls_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(ls_path)?)
                .unwrap_or_else(|_| serde_json::json!({}))
        } else {
            if let Some(p) = ls_path.parent() {
                std::fs::create_dir_all(p).ok();
            }
            serde_json::json!({})
        };
        if !json.is_object() {
            json = serde_json::json!({});
        }
        json["os_crypt"]["encrypted_key"] = serde_json::Value::String(b64);
        std::fs::write(ls_path, serde_json::to_string(&json)?)?;
        Ok(())
    }
}

fn samesite_to_str(v: i64) -> &'static str {
    match v {
        0 => "None",
        1 => "Lax",
        2 => "Strict",
        _ => "unspecified",
    }
}
fn samesite_from_str(s: Option<&str>) -> i64 {
    match s.map(|x| x.to_ascii_lowercase()).as_deref() {
        Some("none") => 0,
        Some("lax") => 1,
        Some("strict") => 2,
        _ => -1,
    }
}

/// Path to the profile's Cookies SQLite DB; Default/ or Default/Network/.
fn cookies_db_path(udd: &Path) -> PathBuf {
    let primary = udd.join("Default").join("Cookies");
    if primary.exists() {
        return primary;
    }
    let alt = udd.join("Default").join("Network").join("Cookies");
    if alt.exists() {
        return alt;
    }
    primary
}

/// Export decrypted cookies.
pub fn export(profile_id: &str) -> Result<Vec<Cookie>> {
    let udd = profile::user_data_dir(profile_id)?;
    let path = cookies_db_path(&udd);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let crypt = Crypt::open(&udd)?;
    // Read-only to avoid WAL write-lock fights with a running browser.
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open {}", path.display()))?;

    let mut stmt = conn.prepare(
        "SELECT host_key, name, value, encrypted_value, path, expires_utc, \
         is_secure, is_httponly, has_expires, samesite FROM cookies",
    )?;
    let rows = stmt.query_map([], |r| {
        let host: String = r.get(0)?;
        let name: String = r.get(1)?;
        let plain: String = r.get(2)?;
        let enc: Vec<u8> = r.get(3)?;
        let path: String = r.get(4)?;
        let expires_utc: i64 = r.get(5)?;
        let is_secure: i64 = r.get(6)?;
        let is_httponly: i64 = r.get(7)?;
        let has_expires: i64 = r.get(8)?;
        let samesite: i64 = r.get(9)?;
        Ok(Cookie {
            value: crypt.decrypt(&enc, &plain),
            domain: host,
            name,
            path,
            expires: if has_expires != 0 {
                Some(chromium_to_unix_secs(expires_utc))
            } else {
                None
            },
            secure: is_secure != 0,
            http_only: is_httponly != 0,
            same_site: Some(samesite_to_str(samesite).to_string()),
        })
    })?;
    let mut out = Vec::new();
    for c in rows {
        out.push(c?);
    }
    Ok(out)
}

/// Import cookies (v10-encrypted). Caller MUST stop the profile first.
pub fn import(profile_id: &str, cookies: &[Cookie]) -> Result<usize> {
    let udd = profile::user_data_dir(profile_id)?;
    let path = cookies_db_path(&udd);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let crypt = Crypt::open(&udd)?;
    let conn =
        rusqlite::Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
    ensure_schema(&conn)?;

    let now = now_chromium();
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO cookies (\
             creation_utc, host_key, top_frame_site_key, name, value, encrypted_value, \
             path, expires_utc, is_secure, is_httponly, last_access_utc, has_expires, \
             is_persistent, priority, samesite, source_scheme, source_port, \
             last_update_utc, source_type, has_cross_site_ancestor) \
             VALUES (?1, ?2, '', ?3, '', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12, ?13, ?14, ?15, 0, 1)",
        )?;
        for c in cookies {
            let enc = crypt.encrypt(&c.domain, &c.value);
            let has_expires = c.expires.is_some();
            let expires_utc = c.expires.map(unix_to_chromium).unwrap_or(0);
            let source_scheme = if c.secure { 2 } else { 1 };
            let source_port = if c.secure { 443 } else { 80 };
            stmt.execute(rusqlite::params![
                now,
                c.domain,
                c.name,
                enc,
                c.path,
                expires_utc,
                c.secure as i64,
                c.http_only as i64,
                now,
                has_expires as i64,
                has_expires as i64,
                samesite_from_str(c.same_site.as_deref()),
                source_scheme,
                source_port,
                now,
            ])?;
        }
    }
    tx.commit()?;
    Ok(cookies.len())
}

/// Create cookies table + meta to match Chromium v24 schema for a fresh DB.
fn ensure_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR);\
         INSERT OR IGNORE INTO meta (key, value) VALUES ('version', '24');\
         INSERT OR IGNORE INTO meta (key, value) VALUES ('last_compatible_version', '24');\
         CREATE TABLE IF NOT EXISTS cookies (\
            creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL, \
            top_frame_site_key TEXT NOT NULL, name TEXT NOT NULL, value TEXT NOT NULL, \
            encrypted_value BLOB NOT NULL, path TEXT NOT NULL, expires_utc INTEGER NOT NULL, \
            is_secure INTEGER NOT NULL, is_httponly INTEGER NOT NULL, last_access_utc INTEGER NOT NULL, \
            has_expires INTEGER NOT NULL, is_persistent INTEGER NOT NULL, priority INTEGER NOT NULL, \
            samesite INTEGER NOT NULL, source_scheme INTEGER NOT NULL, source_port INTEGER NOT NULL, \
            last_update_utc INTEGER NOT NULL, source_type INTEGER NOT NULL, \
            has_cross_site_ancestor INTEGER NOT NULL);\
         CREATE UNIQUE INDEX IF NOT EXISTS cookies_unique_index ON cookies (\
            host_key, top_frame_site_key, has_cross_site_ancestor, name, path, source_scheme, source_port);",
    )?;
    Ok(())
}
