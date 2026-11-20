//! The Widevine CDM, fetched the way Chrome's component updater fetches it:
//! Google's update server names the current build, Google's download server
//! hands out a CRX3 that must be signed for Widevine's id with the offered
//! hash and size. Through the profile's proxy when it has one, straight out if
//! not. The engine is built without BUNDLE_WIDEVINE_CDM, so each profile gets
//! the cached CDM in `<udd>/WidevineCdm/<version>/` before it starts. Mirrors
//! the launcher's `widevine.rs`, `crx.rs` and the widevine functions of `launch.rs`.

use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use ring::signature::{self, UnparsedPublicKey, VerificationAlgorithm};
use serde_json::{json, Value};

use crate::proxy::{ParsedProxy, ProxyScheme};

/// Widevine's component id: the SHA-256 of its signing key, a..p.
pub const WIDEVINE_ID: &str = "oimompecagnajdejgnnjijobebaeigek";
const UPDATE_URL: &str = "https://update.googleapis.com/service/update2/json";
/// The package is about 20 MB; anything far larger is not it.
const MAX_PACKAGE: u64 = 64 << 20;
const MAX_FILES: usize = 64;
/// How often the cached CDM is checked against Google's, as Chrome checks its components.
pub const WIDEVINE_RECHECK: Duration = Duration::from_secs(5 * 3600);
/// How long a start waits for a first CDM; it starts without one after that.
pub const WIDEVINE_FIRST_WAIT: Duration = Duration::from_secs(45);
const HINT_FILE: &str = "latest-component-updated-widevine-cdm";

// ---- the update check ----

/// This computer as Chrome's updater names it: @os, os.platform, arch, os.arch.
fn platform() -> (&'static str, &'static str, &'static str, &'static str) {
    let arm = cfg!(target_arch = "aarch64");
    if cfg!(target_os = "macos") {
        ("mac", "Mac OS X", if arm { "arm64" } else { "x64" }, if arm { "arm64" } else { "x86_64" })
    } else if cfg!(windows) {
        ("win", "Windows", if arm { "arm64" } else { "x64" }, if arm { "arm64" } else { "x86_64" })
    } else {
        ("linux", "Linux", if arm { "arm64" } else { "x64" }, if arm { "aarch64" } else { "x86_64" })
    }
}

/// The OS release Chrome sends: the server answers "noupdate" without one.
#[cfg(target_os = "macos")]
fn os_release() -> String {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    let name = b"kern.osproductversion\0";
    // SAFETY: a NUL-terminated name, a buffer of the size given.
    let rc = unsafe {
        libc::sysctlbyname(name.as_ptr().cast(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
    };
    if rc != 0 || len == 0 {
        return "15.0".into();
    }
    let v = String::from_utf8_lossy(&buf[..len.min(buf.len())]).trim_end_matches('\0').to_string();
    if v.matches('.').count() == 1 {
        format!("{v}.0")
    } else {
        v
    }
}

#[cfg(target_os = "linux")]
fn os_release() -> String {
    // SAFETY: uname fills the struct it is given.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return "6.0.0".into();
    }
    // SAFETY: uname NUL-terminates release.
    let release = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) }.to_string_lossy().to_string();
    release.split(|c: char| !(c.is_ascii_digit() || c == '.')).next().unwrap_or("6.0.0").to_string()
}

#[cfg(windows)]
fn os_release() -> String {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY,
    };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let key = wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let dword = |name: &str| -> Option<u32> {
        let name = wide(name);
        let mut v = 0u32;
        let mut size = 4u32;
        // SAFETY: NUL-terminated names, a DWORD-sized buffer.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
                std::ptr::null_mut(),
                (&mut v as *mut u32).cast(),
                &mut size,
            )
        };
        (rc == 0).then_some(v)
    };
    let build = {
        let name = wide("CurrentBuildNumber");
        let mut buf = [0u16; 32];
        let mut size = std::mem::size_of_val(&buf) as u32;
        // SAFETY: as above, with a buffer of the size given.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        let n = (size as usize / 2).min(buf.len());
        (rc == 0).then(|| String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string())
    };
    format!(
        "{}.{}.{}.{}",
        dword("CurrentMajorVersionNumber").unwrap_or(10),
        dword("CurrentMinorVersionNumber").unwrap_or(0),
        build.unwrap_or_else(|| "19045".into()),
        dword("UBR").unwrap_or(0)
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn os_release() -> String {
    "0.0.0".into()
}

/// A random UUID in braces, as Chrome writes its request and session ids.
fn braced_uuid() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{{{}-{}-{}-{}-{}}}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// The update check, as Chrome 158's component updater sends it (protocol 4.0).
pub(crate) fn request_body(chrome: &str, have: Option<&str>, background: bool) -> Value {
    let (os, platform, arch, os_arch) = platform();
    json!({ "request": {
        "@os": os,
        "@updater": "chrome",
        "acceptformat": "crx3,download,puff,run,xz,zucc",
        "apps": [{
            "appid": WIDEVINE_ID,
            "enabled": true,
            "installsource": if background { "" } else { "ondemand" },
            "ping": { "r": -2 },
            "updatecheck": {},
            "version": have.unwrap_or("0.0.0.0"),
        }],
        "arch": arch,
        "dedup": "cr",
        "ismachine": false,
        "os": { "arch": os_arch, "platform": platform, "version": os_release() },
        "prodchannel": "stable",
        "prodversion": chrome,
        "protocol": "4.0",
        "requestid": braced_uuid(),
        "sessionid": braced_uuid(),
        "updaterchannel": "stable",
        "updaterversion": chrome,
    }})
}

/// What the server offered: the version, its https download urls, hash and size.
#[derive(Debug, PartialEq)]
pub(crate) struct Offer {
    pub version: String,
    pub urls: Vec<String>,
    pub sha256: String,
    pub size: u64,
}

/// None for "noupdate"; an error for anything else that is not an offer.
pub(crate) fn parse_offer(text: &str) -> Result<Option<Offer>> {
    let body: Value =
        serde_json::from_str(text.trim_start_matches(")]}'").trim()).context("the update server's answer is not JSON")?;
    let app = &body["response"]["apps"][0];
    let check = &app["updatecheck"];
    match check["status"].as_str() {
        Some("ok") => {}
        Some("noupdate") => return Ok(None),
        other => bail!("the update server answered {}", other.or(app["status"].as_str()).unwrap_or("nothing")),
    }
    let version = check["nextversion"].as_str().context("no version in the offer")?.to_string();
    let ops = check["pipelines"][0]["operations"].as_array().context("no pipeline in the offer")?;
    let download = ops.iter().find(|o| o["type"] == "download").context("no download in the offer")?;
    let urls: Vec<String> = download["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| u["url"].as_str())
        .filter(|u| u.starts_with("https://"))
        .map(str::to_string)
        .collect();
    let sha256 = download["out"]["sha256"].as_str().context("no hash in the offer")?.to_string();
    let size = download["size"].as_u64().context("no size in the offer")?;
    if urls.is_empty() {
        bail!("no https download in the offer");
    }
    Ok(Some(Offer { version, urls, sha256, size }))
}

// ---- CRX3, checked as Chrome's crx_verifier.cc checks it ----

/// A header bigger than this is not a real one.
const MAX_HEADER: usize = 1 << 20;
const SIGNATURE_CONTEXT: &[u8] = b"CRX3 SignedData\x00";
/// Zip end-of-directory tokens: in a header they could make unzip see another archive.
const ZIP_TOKENS: [&[u8]; 3] = [b"PK\x05\x06", b"PK\x06\x07", b"PK\x06\x06"];

/// Why a package was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    NotCrx3,
    HeaderSize,
    ZipTokenInHeader,
    BadHeader,
    BadKey,
    BadSignature,
    NoDeveloperProof,
    WrongId { want: String, got: String },
    WrongHash,
    WrongSize { want: u64, got: u64 },
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reject::NotCrx3 => f.write_str("not a CRX3 package"),
            Reject::HeaderSize => f.write_str("the CRX header's size is wrong"),
            Reject::ZipTokenInHeader => f.write_str("the CRX header holds a zip marker"),
            Reject::BadHeader => f.write_str("the CRX header cannot be read"),
            Reject::BadKey => f.write_str("a key in the CRX cannot be read"),
            Reject::BadSignature => f.write_str("the CRX signature does not match its contents"),
            Reject::NoDeveloperProof => f.write_str("the CRX is not signed by its developer's key"),
            Reject::WrongId { want, got } => write!(f, "the package is {got}, not {want}"),
            Reject::WrongHash => f.write_str("the download does not match the server's hash"),
            Reject::WrongSize { want, got } => write!(f, "the download is {got} bytes, the server said {want}"),
        }
    }
}

impl std::error::Error for Reject {}

/// What the package must be, beyond a valid CRX3.
#[derive(Debug, Clone, Default)]
pub struct Expect<'a> {
    /// The id it must be signed for.
    pub id: Option<&'a str>,
    /// Hex SHA-256 and size of the whole file, as update2 gave them.
    pub sha256: Option<&'a str>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // all of what crx.rs returns; the launch path reads the offset
pub struct Verified {
    /// The id the package is signed for, a..p.
    pub crx_id: String,
    /// The developer's public key (SPKI DER).
    pub developer_key: Vec<u8>,
    /// Where the zip starts.
    pub archive_offset: usize,
    /// Hex SHA-256 of the whole file.
    pub sha256: String,
}

fn sha256(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, data).as_ref().to_vec()
}

/// The first 16 bytes of a hash, one letter a..p per nibble.
pub fn id_from_hash(hash: &[u8]) -> String {
    hash.iter().take(16).flat_map(|b| [b >> 4, b & 0x0f]).map(|n| char::from(b'a' + n)).collect()
}

/// The id a public key (SubjectPublicKeyInfo DER) gives.
pub fn id_from_key(der: &[u8]) -> String {
    id_from_hash(&sha256(der))
}

/// Every proof must verify and one must be the developer's key for the signed id.
pub fn verify_crx(bytes: &[u8], expect: &Expect) -> Result<Verified, Reject> {
    let sha256_hex: String = sha256(bytes).iter().map(|b| format!("{b:02x}")).collect();
    if let Some(want) = expect.size {
        if want != bytes.len() as u64 {
            return Err(Reject::WrongSize { want, got: bytes.len() as u64 });
        }
    }
    if let Some(want) = expect.sha256 {
        if !want.eq_ignore_ascii_case(&sha256_hex) {
            return Err(Reject::WrongHash);
        }
    }
    if bytes.len() < 12 || &bytes[..4] != b"Cr24" || bytes[4..8] != 3u32.to_le_bytes() {
        return Err(Reject::NotCrx3);
    }
    let header_size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if header_size > MAX_HEADER || bytes.len() - 12 < header_size {
        return Err(Reject::HeaderSize);
    }
    let header = &bytes[12..12 + header_size];
    let archive_offset = 12 + header_size;
    if ZIP_TOKENS.iter().any(|t| header.windows(4).any(|w| w == *t)) {
        return Err(Reject::ZipTokenInHeader);
    }

    // CrxFileHeader: proofs 2 (RSA) and 3 (ECDSA), signed_header_data 10000.
    let (mut proofs, mut signed) = (Vec::new(), &[][..]);
    for (field, data) in fields(header).ok_or(Reject::BadHeader)? {
        match field {
            2 | 3 => {
                let (mut key, mut sig) = (&[][..], &[][..]);
                for (f, d) in fields(data).ok_or(Reject::BadHeader)? {
                    match f {
                        1 => key = d,
                        2 => sig = d,
                        _ => {}
                    }
                }
                proofs.push((field == 2, key, sig));
            }
            10000 => signed = data,
            _ => {}
        }
    }
    let mut crx_id = None;
    for (f, d) in fields(signed).ok_or(Reject::BadHeader)? {
        if f == 1 {
            crx_id = Some(d);
        }
    }
    // Chrome compares 32 letters; any other length matches no key.
    let declared = crx_id.filter(|id| id.len() == 16).map(id_from_hash);

    let mut developer_key = None;
    let mut checks = Vec::new();
    for (rsa, key, sig) in proofs {
        if declared.as_deref() == Some(id_from_key(key).as_str()) {
            developer_key = Some(key);
        }
        let (alg, raw) = spki(key, rsa).ok_or(Reject::BadKey)?;
        checks.push((alg, raw, sig));
    }
    let (Some(crx_id), Some(developer_key)) = (declared, developer_key) else {
        return Err(Reject::NoDeveloperProof);
    };

    // Every proof covers context, le32(len), signed_header_data and the archive.
    let archive = &bytes[archive_offset..];
    let mut msg = Vec::with_capacity(SIGNATURE_CONTEXT.len() + 4 + signed.len() + archive.len());
    msg.extend_from_slice(SIGNATURE_CONTEXT);
    msg.extend_from_slice(&(signed.len() as u32).to_le_bytes());
    msg.extend_from_slice(signed);
    msg.extend_from_slice(archive);
    for (alg, raw, sig) in &checks {
        UnparsedPublicKey::new(*alg, raw).verify(&msg, sig).map_err(|_| Reject::BadSignature)?;
    }
    if let Some(want) = expect.id {
        if want != crx_id {
            return Err(Reject::WrongId { want: want.to_string(), got: crx_id });
        }
    }
    Ok(Verified { crx_id, developer_key: developer_key.to_vec(), archive_offset, sha256: sha256_hex })
}

/// The length-delimited fields of a protobuf message, in order; others are
/// skipped as Chrome's parser skips unknown fields. None if malformed.
fn fields(mut b: &[u8]) -> Option<Vec<(u64, &[u8])>> {
    fn varint(b: &mut &[u8]) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = b.split_first()?;
            *b = rest;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        let tag = varint(&mut b)?;
        if tag >> 3 == 0 {
            return None;
        }
        let skip = match tag & 7 {
            0 => {
                varint(&mut b)?;
                0
            }
            1 => 8,
            2 => {
                let len = usize::try_from(varint(&mut b)?).ok()?;
                out.push((tag >> 3, b.get(..len)?));
                len
            }
            5 => 4,
            _ => return None,
        };
        b = b.get(skip..)?;
    }
    Some(out)
}

/// One DER element: tag, contents, what follows. Strict lengths, as BoringSSL reads them.
fn der(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, b) = b.split_first()?;
    let (&first, mut b) = b.split_first()?;
    let len = if first < 0x80 {
        usize::from(first)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || *b.first()? == 0 {
            return None;
        }
        let mut len = 0usize;
        for &x in b.get(..n)? {
            len = len << 8 | usize::from(x);
        }
        b = &b[n..];
        if len < 0x80 {
            return None;
        }
        len
    };
    Some((tag, b.get(..len)?, &b[len..]))
}

const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];

/// A SubjectPublicKeyInfo as what ring verifies with: the algorithm for its
/// kind of key and the key's raw form. None if unreadable or of the other kind.
fn spki(key: &[u8], rsa: bool) -> Option<(&'static dyn VerificationAlgorithm, &[u8])> {
    let (0x30, body, []) = der(key)? else { return None };
    let (0x30, alg, rest) = der(body)? else { return None };
    let (0x03, bits, []) = der(rest)? else { return None };
    let (0x06, oid, params) = der(alg)? else { return None };
    let (&0, raw) = bits.split_first()? else { return None };
    match (rsa, oid) {
        (true, OID_RSA) if params == [0x05, 0x00] => Some((&signature::RSA_PKCS1_1024_8192_SHA256_FOR_LEGACY_USE_ONLY, raw)),
        (false, OID_EC) => match der(params)? {
            (0x06, OID_P256, []) => Some((&signature::ECDSA_P256_SHA256_ASN1, raw)),
            (0x06, OID_P384, []) => Some((&signature::ECDSA_P384_SHA256_ASN1, raw)),
            _ => None,
        },
        _ => None,
    }
}

// ---- download and install into the cache ----

/// Userinfo percent-encoded: only unreserved characters stay as they are.
fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => char::from(b).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn client(proxy: Option<&ParsedProxy>, chrome: &str) -> Result<reqwest::Client> {
    let major = chrome.split('.').next().filter(|m| !m.is_empty()).unwrap_or("158");
    let mut builder = reqwest::Client::builder()
        .user_agent(format!("Mozilla/5.0 Chrome/{major}.0.0.0"))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180));
    match proxy.filter(|p| !p.host.is_empty()) {
        Some(p) => {
            let scheme = match p.scheme {
                ProxyScheme::Socks5 => "socks5h",
                ProxyScheme::Http => "http",
                ProxyScheme::Https => "https",
            };
            let url = if p.username.is_empty() && p.password.is_empty() {
                format!("{scheme}://{}:{}", p.host, p.port)
            } else {
                format!("{scheme}://{}:{}@{}:{}", pct(&p.username), pct(&p.password), p.host, p.port)
            };
            builder = builder.proxy(reqwest::Proxy::all(&url).context("bad proxy URL")?);
        }
        None => builder = builder.no_proxy(),
    }
    Ok(builder.build()?)
}

/// Fetches the current CDM into `cache` when the server has a newer one than
/// `have`. Returns the version put there, or None when `have` is current.
pub async fn fetch_widevine(
    proxy: Option<&ParsedProxy>,
    chrome: &str,
    have: Option<&str>,
    cache: &Path,
    background: bool,
) -> Result<Option<String>> {
    let client = client(proxy, chrome)?;
    let resp = client
        .post(UPDATE_URL)
        .header("Content-Type", "application/json")
        .header("X-Goog-Update-AppId", WIDEVINE_ID)
        .header("X-Goog-Update-Interactivity", if background { "bg" } else { "fg" })
        .header("X-Goog-Update-Updater", format!("chrome-{chrome}"))
        .body(request_body(chrome, have, background).to_string())
        .send()
        .await
        .context("the update server could not be reached")?;
    if !resp.status().is_success() {
        bail!("the update server answered HTTP {}", resp.status());
    }
    let Some(offer) = parse_offer(&resp.text().await?)? else { return Ok(None) };
    if offer.size > MAX_PACKAGE {
        bail!("the offered package is {} MB, more than a CDM", offer.size >> 20);
    }
    let mut last = None;
    for url in &offer.urls {
        match download(&client, url, &offer).await {
            Ok((bytes, v)) => {
                install_package(&bytes, v.archive_offset, &offer.version, cache)?;
                return Ok(Some(offer.version));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no download worked")))
}

async fn download(client: &reqwest::Client, url: &str, offer: &Offer) -> Result<(Vec<u8>, Verified)> {
    let mut resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {} from {url}", resp.status());
    }
    let mut bytes = Vec::with_capacity(offer.size as usize);
    while let Some(chunk) = resp.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > offer.size {
            bail!("the download from {url} is larger than the offer said");
        }
    }
    let expect = Expect { id: Some(WIDEVINE_ID), sha256: Some(&offer.sha256), size: Some(offer.size) };
    let v = verify_crx(&bytes, &expect).map_err(|r| anyhow!("the package failed its check: {r}"))?;
    Ok((bytes, v))
}

/// `<path>` with `suffix` appended to its last component.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Unpacks a verified package beside `cache`, checks its manifest names the
/// offered version, then puts it in the cache's place.
pub(crate) fn install_package(bytes: &[u8], archive_offset: usize, version: &str, cache: &Path) -> Result<()> {
    let staging = sibling(cache, &format!(".incoming-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| {
        unzip_within(&bytes[archive_offset..], &staging, MAX_PACKAGE * 2, MAX_FILES)?;
        if cdm_version_of(&staging).as_deref() != Some(version) {
            if staging.join("manifest.json").exists() {
                bail!("the package's manifest names another version than the offer");
            }
            bail!("no manifest.json in the package");
        }
        let _ = std::fs::remove_dir_all(cache);
        std::fs::rename(&staging, cache)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn unzip_within(archive: &[u8], dst: &Path, max: u64, most: usize) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).context("not a zip/crx archive")?;
    if zip.len() > most {
        bail!("the archive holds more than {most} files");
    }
    let too_big = || anyhow!("the package unpacks to more than {} MB", max >> 20);
    let said = (0..zip.len()).filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.size())).fold(0u64, u64::saturating_add);
    if said > max {
        return Err(too_big());
    }
    std::fs::create_dir_all(dst)?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let out = dst.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
        // What it really holds, whatever its header said.
        total += std::io::copy(&mut f.take(max - total + 1), &mut file)?;
        if total > max {
            return Err(too_big());
        }
    }
    Ok(())
}

// ---- the cache and each profile ----

/// Version string as numbers, so "4.10.2891.0" sorts above "4.9.9999.0".
fn cdm_version(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// A version usable as a folder name: digits and dots only.
fn plain_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() <= 4 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

pub fn cdm_version_of(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    manifest.get("version")?.as_str().map(String::from)
}

fn checked_lately(stamp: &Path) -> bool {
    std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < WIDEVINE_RECHECK)
}

/// How a check reaches Google; tests stand their own in.
pub(crate) type Fetcher = Arc<
    dyn Fn(Option<ParsedProxy>, String, Option<String>, PathBuf, bool) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + Send>>
        + Send
        + Sync,
>;

fn google() -> Fetcher {
    Arc::new(|proxy, chrome, have, cache, background| {
        Box::pin(async move { fetch_widevine(proxy.as_ref(), &chrome, have.as_deref(), &cache, background).await })
    })
}

/// A first CDM is waited for (up to 45 s); a cached one is checked for a newer
/// build in the background, every 5 hours. `cache` is `<cache_dir>/widevine-cdm`.
pub async fn ensure_widevine(cache: &Path, chrome: &str, proxy: Option<&ParsedProxy>) {
    ensure_with(cache, chrome, proxy, google(), WIDEVINE_FIRST_WAIT).await
}

pub(crate) async fn ensure_with(cache: &Path, chrome: &str, proxy: Option<&ParsedProxy>, fetch: Fetcher, first_wait: Duration) {
    let stamp = sibling(cache, ".checked");
    let have = cdm_version_of(cache);
    if have.is_some() && checked_lately(&stamp) {
        return;
    }
    let (cache, chrome, proxy) = (cache.to_path_buf(), chrome.to_string(), proxy.cloned());
    let check = async move {
        // One check at a time, however many profiles start together.
        static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _one = ONE.lock().await;
        let have = cdm_version_of(&cache);
        if have.is_some() && checked_lately(&stamp) {
            return;
        }
        let background = have.is_some();
        match fetch(proxy, chrome, have, cache, background).await {
            Ok(got) => {
                if let Some(v) = got {
                    eprintln!("[shardx] Widevine CDM {v} from Google");
                }
                let _ = std::fs::write(&stamp, b"");
            }
            Err(e) => eprintln!("[shardx] Widevine check failed: {e:#}"),
        }
    };
    if have.is_none() {
        if tokio::time::timeout(first_wait, check).await.is_err() {
            eprintln!("[shardx] no Widevine CDM yet: Google did not answer in time; this profile starts without one");
        }
    } else {
        tokio::spawn(check);
    }
}

/// Take a CDM the engine fetched for itself into the cache when it is newer,
/// so the next profile starts with it. Returns the version taken, if any.
pub fn harvest_widevine(cache: &Path, udd: &Path) -> Result<Option<String>> {
    let root = udd.join("WidevineCdm");
    if !root.is_dir() {
        return Ok(None);
    }
    let mut newest: Option<(Vec<u64>, String, PathBuf)> = None;
    for entry in std::fs::read_dir(&root)?.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        // The engine names each folder by its version; anything else is not one of its.
        let Some(version) = cdm_version_of(&p) else { continue };
        if !plain_version(&version) || entry.file_name().to_str() != Some(version.as_str()) {
            continue;
        }
        let parsed = cdm_version(&version);
        if newest.as_ref().map_or(true, |(best, _, _)| parsed > *best) {
            newest = Some((parsed, version, p));
        }
    }
    let Some((found, version, src)) = newest else { return Ok(None) };
    if cdm_version_of(cache).is_some_and(|have| cdm_version(&have) >= found) {
        return Ok(None);
    }
    // Into a sibling first: a half-copied cache is worse than none.
    let staging = sibling(cache, &format!(".incoming-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = copy_dir_recursive(&src, &staging).and_then(|()| {
        let _ = std::fs::remove_dir_all(cache);
        Ok(std::fs::rename(&staging, cache)?)
    });
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    eprintln!("[shardx] Widevine CDM {version} cached from a profile");
    Ok(Some(version))
}

/// Copies the cached CDM into `<udd>/WidevineCdm/<version>/`, the layout
/// Chromium's component installer reads, and writes the Linux hint. Returns
/// the version folder, or None while nothing is cached.
pub fn install_widevine(cache: &Path, udd: &Path) -> Result<Option<PathBuf>> {
    if !cache.exists() {
        return Ok(None);
    }
    let version = cdm_version_of(cache)
        .filter(|v| plain_version(v))
        .ok_or_else(|| anyhow!("the cached CDM at {} names no usable version", cache.display()))?;
    let root = udd.join("WidevineCdm");
    let versioned = root.join(&version);
    if !versioned.exists() {
        // A flat layout an older SDK or launcher left goes.
        if root.join("manifest.json").exists() {
            for stray in ["manifest.json", "LICENSE", "_platform_specific"] {
                let p = root.join(stray);
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else if p.exists() {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        let staging = root.join(format!(".{version}.incoming"));
        let _ = std::fs::remove_dir_all(&staging);
        let result = copy_dir_recursive(cache, &staging).and_then(|()| Ok(std::fs::rename(&staging, &versioned)?));
        let _ = std::fs::remove_dir_all(&staging);
        result.with_context(|| format!("copy {} to {}", cache.display(), versioned.display()))?;
    }
    write_widevine_hint(&root, &versioned)?;
    Ok(Some(versioned))
}

/// The hint Chrome on Linux loads its component CDM from at start, before the
/// sandbox closes; harmless on other platforms.
pub fn write_widevine_hint(root: &Path, versioned: &Path) -> Result<()> {
    let hint = root.join(HINT_FILE);
    let abs = if versioned.is_absolute() { versioned.to_path_buf() } else { std::env::current_dir()?.join(versioned) };
    let body = json!({ "Path": abs.to_string_lossy() }).to_string();
    if std::fs::read_to_string(&hint).ok().as_deref() != Some(body.as_str()) {
        std::fs::write(&hint, body)?;
    }
    Ok(())
}

/// A plain copy; links are resolved so the copy stands on its own.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_symlink() {
            let target = std::fs::read_link(&from)?;
            let resolved = if target.is_absolute() { target } else { from.parent().unwrap_or(src).join(target) };
            if resolved.is_dir() {
                copy_dir_recursive(&resolved, &to)?;
            } else {
                std::fs::copy(&resolved, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
