//! The Widevine CDM, fetched the way Chrome's component updater fetches it:
//! Google's update server names the current build for this platform, Google's
//! download server hands out the package, and the package must be a CRX3
//! signed for Widevine's id with the hash and size the server gave. Through
//! the proxy of the profile being started, as the engine's own updater would
//! go; straight out for a profile without one, as the engine would too.
//!
//! Chrome ships with its CDM; this engine does not (BUNDLE_WIDEVINE_CDM is
//! off), so a new profile's first pages found no Widevine at all. anti.bot
//! counts that against a Mac Chrome and named the browser Incogniton.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use super::crx;
use crate::space::local::proxy::{reqwest_proxy, ProxyEntry};

/// Widevine's component id: the SHA-256 of its signing key, a..p.
pub const WIDEVINE_ID: &str = "oimompecagnajdejgnnjijobebaeigek";
const UPDATE_URL: &str = "https://update.googleapis.com/service/update2/json";
/// The package is about 20 MB; anything far larger is not it.
const MAX_PACKAGE: u64 = 64 << 20;
const MAX_FILES: usize = 64;

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
    let name = c"kern.osproductversion";
    // SAFETY: a NUL-terminated name, a buffer of the size given.
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 || len == 0 {
        return "15.0".into();
    }
    let v = String::from_utf8_lossy(&buf[..len]).trim_end_matches('\0').to_string();
    if v.matches('.').count() == 1 { format!("{v}.0") } else { v }
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
            RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
                std::ptr::null_mut(), (&mut v as *mut u32).cast(), &mut size)
        };
        (rc == 0).then_some(v)
    };
    let build = {
        let name = wide("CurrentBuildNumber");
        let mut buf = [0u16; 32];
        let mut size = std::mem::size_of_val(&buf) as u32;
        // SAFETY: as above, with a buffer of the size given.
        let rc = unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
                std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size)
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

/// The update check, as Chrome 158's component updater sends it (protocol 4.0).
fn request_body(chrome: &str, have: Option<&str>, background: bool) -> Value {
    let (os, platform, arch, os_arch) = platform();
    let id = || format!("{{{}}}", uuid::Uuid::new_v4());
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
        "requestid": id(),
        "sessionid": id(),
        "updaterchannel": "stable",
        "updaterversion": chrome,
    }})
}

/// What the server offered: the version, its https download urls, hash and size.
#[derive(Debug, PartialEq)]
struct Offer {
    version: String,
    urls: Vec<String>,
    sha256: String,
    size: u64,
}

/// None for "noupdate"; an error for anything else that is not an offer.
fn parse_offer(text: &str) -> Result<Option<Offer>> {
    let body: Value = serde_json::from_str(text.trim_start_matches(")]}'").trim()).context("the update server's answer is not JSON")?;
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

fn client(proxy: Option<&ProxyEntry>, chrome: &str) -> Result<reqwest::Client> {
    let major = chrome.split('.').next().unwrap_or("158");
    let mut builder = reqwest::Client::builder()
        .user_agent(format!("Mozilla/5.0 Chrome/{major}.0.0.0"))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180));
    if let Some(entry) = proxy.filter(|p| !p.host.is_empty()) {
        builder = builder.proxy(reqwest_proxy(entry)?);
    }
    Ok(builder.build()?)
}

/// Fetches the current CDM into `cache` when the server has a newer one than
/// `have`. Returns the version put there, or None when `have` is current.
pub async fn fetch(proxy: Option<&ProxyEntry>, chrome: &str, have: Option<&str>, cache: &Path, background: bool) -> Result<Option<String>> {
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
            Ok(bytes) => {
                install(&bytes, &offer, cache)?;
                return Ok(Some(offer.version));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no download worked")))
}

async fn download(client: &reqwest::Client, url: &str, offer: &Offer) -> Result<Vec<u8>> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {} from {url}", resp.status());
    }
    let bytes = resp.bytes().await?.to_vec();
    let expect = crx::Expect { id: Some(WIDEVINE_ID), publisher: None, sha256: Some(&offer.sha256), size: Some(offer.size) };
    crx::verify(&bytes, &expect).map_err(|r| anyhow::anyhow!("the package failed its check: {r}"))?;
    Ok(bytes)
}

/// Unpacks a verified package beside `cache`, checks its manifest names the
/// offered version, then puts it in the cache's place.
fn install(bytes: &[u8], offer: &Offer, cache: &Path) -> Result<()> {
    let expect = crx::Expect { id: Some(WIDEVINE_ID), ..Default::default() };
    let v = crx::verify(bytes, &expect).map_err(|r| anyhow::anyhow!("the package failed its check: {r}"))?;
    let staging = cache.with_extension("incoming");
    let _ = std::fs::remove_dir_all(&staging);
    super::unzip_within(&bytes[v.archive_offset..], &staging, MAX_PACKAGE * 2, MAX_FILES)?;
    let manifest: Value = serde_json::from_slice(&std::fs::read(staging.join("manifest.json")).context("no manifest.json in the package")?)?;
    if manifest["version"].as_str() != Some(offer.version.as_str()) {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("the package's manifest names another version than the offer");
    }
    let _ = std::fs::remove_dir_all(cache);
    std::fs::rename(&staging, cache)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_names_this_platform_and_its_release() {
        let body = request_body("158.0.8127.36", None, false);
        let r = &body["request"];
        assert_eq!(r["apps"][0]["appid"], WIDEVINE_ID);
        assert_eq!(r["apps"][0]["version"], "0.0.0.0");
        assert_eq!(r["protocol"], "4.0");
        assert_eq!(r["prodversion"], "158.0.8127.36");
        let release = r["os"]["version"].as_str().unwrap();
        assert!(release.chars().next().is_some_and(|c| c.is_ascii_digit()), "{release}");
        assert_eq!(request_body("158", Some("4.10.3112.0"), true)["request"]["apps"][0]["version"], "4.10.3112.0");
    }

    /// By hand: `cargo test --lib widevine_live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "reaches Google's update and download servers"]
    async fn widevine_live_fetch_puts_a_signed_cdm_in_place() {
        let dir = std::env::temp_dir().join(format!("shardx-widevine-{}", std::process::id()));
        let cache = dir.join("widevine-cdm");
        let got = fetch(None, "158.0.8127.36", None, &cache, false).await.unwrap();
        let v = got.expect("an offer for a profile with none");
        let manifest: Value = serde_json::from_slice(&std::fs::read(cache.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["version"].as_str(), Some(v.as_str()));
        let libs: Vec<_> = walk(&cache.join("_platform_specific"));
        println!("widevine {v}: {libs:?}");
        assert!(libs.iter().any(|p| p.contains("widevinecdm")), "{libs:?}");
        // The same version again is current.
        assert_eq!(fetch(None, "158.0.8127.36", Some(&v), &cache, true).await.unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// By hand, with a proxy line in any form the proxy list accepts:
    /// `SHARDX_TEST_PROXY=host:port:user:pass cargo test --lib widevine_live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "reaches Google's servers through the proxy in SHARDX_TEST_PROXY"]
    async fn widevine_live_fetch_goes_through_the_profile_proxy() {
        use crate::space::local::proxy::{parse_single, ProxyKind};
        let Ok(line) = std::env::var("SHARDX_TEST_PROXY") else { return };
        let base = parse_single(&line).expect("a proxy line");
        let dir = std::env::temp_dir().join(format!("shardx-widevine-proxy-{}", std::process::id()));
        // A proxy that is not there fails the fetch: nothing goes around it.
        let dead = ProxyEntry { host: "127.0.0.1".into(), port: 9, ..base.clone() };
        assert!(fetch(Some(&dead), "158.0.8127.36", None, &dir.join("dead"), false).await.is_err());
        for kind in [ProxyKind::Socks5, ProxyKind::Http] {
            let proxy = ProxyEntry { kind: kind.clone(), ..base.clone() };
            let cache = dir.join(format!("{kind:?}"));
            let v = fetch(Some(&proxy), "158.0.8127.36", None, &cache, false).await.unwrap().expect("an offer");
            assert!(walk(&cache.join("_platform_specific")).iter().any(|p| p.contains("widevinecdm")));
            println!("widevine {v} through {kind:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn walk(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() { out.extend(walk(&p)) } else { out.push(p.to_string_lossy().to_string()) }
        }
        out
    }

    #[test]
    fn an_offer_is_read_and_noupdate_is_none() {
        let ok = r#")]}'
{"response":{"protocol":"4.0","apps":[{"appid":"oimompecagnajdejgnnjijobebaeigek","status":"ok","updatecheck":{"nextversion":"4.10.3112.0","status":"ok","pipelines":[{"pipeline_id":"1:full","operations":[{"size":20423379,"type":"download","out":{"sha256":"6bf3"},"urls":[{"url":"http://edgedl.me.gvt1.com/a.crx3"},{"url":"https://edgedl.me.gvt1.com/a.crx3"}]},{"type":"crx3"}]}]}}]}}"#;
        assert_eq!(
            parse_offer(ok).unwrap(),
            Some(Offer { version: "4.10.3112.0".into(), urls: vec!["https://edgedl.me.gvt1.com/a.crx3".into()], sha256: "6bf3".into(), size: 20423379 })
        );
        let none = r#")]}'
{"response":{"apps":[{"appid":"x","status":"ok","updatecheck":{"status":"noupdate"}}]}}"#;
        assert_eq!(parse_offer(none).unwrap(), None);
        assert!(parse_offer(r#"{"response":{"apps":[{"status":"error-unknownApplication"}]}}"#).is_err());
    }
}
