use super::*;
use std::sync::Mutex;
use std::time::SystemTime;

const FIXTURES: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/crx3.json"));

/// A field of the CRX3 fixtures: packages signed by OpenSSL with throwaway keys (see `_about`).
fn fixture_str(name: &str) -> String {
    let all: Value = serde_json::from_str(FIXTURES).unwrap();
    all[name].as_str().unwrap().to_string()
}

fn fixture(name: &str) -> Vec<u8> {
    let hex = fixture_str(name);
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

/// A fresh, empty folder under the system's temp dir.
fn temp(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("shardx-wv-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fake_cdm(dir: &Path, version: &str) {
    std::fs::create_dir_all(dir.join("_platform_specific").join("mac_arm64")).unwrap();
    std::fs::write(dir.join("manifest.json"), json!({ "name": "WidevineCdm", "version": version }).to_string()).unwrap();
    std::fs::write(dir.join("_platform_specific").join("mac_arm64").join("libwidevinecdm.dylib"), "cdm").unwrap();
}

#[test]
fn the_request_names_this_platform_its_release_and_the_engine() {
    let body = request_body("158.0.8127.36", None, false);
    let r = &body["request"];
    assert_eq!(r["apps"][0]["appid"], WIDEVINE_ID);
    assert_eq!(r["apps"][0]["version"], "0.0.0.0");
    assert_eq!(r["apps"][0]["installsource"], "ondemand");
    assert_eq!(r["protocol"], "4.0");
    assert_eq!(r["prodversion"], "158.0.8127.36");
    assert_eq!(r["updaterversion"], "158.0.8127.36");
    let release = r["os"]["version"].as_str().unwrap();
    assert!(release.split('.').count() >= 2 && release.split('.').all(|p| p.parse::<u32>().is_ok()), "{release}");
    let id = r["requestid"].as_str().unwrap();
    assert!(id.len() == 38 && id.starts_with('{') && id.ends_with('}') && id.as_bytes()[15] == b'4', "{id}");
    let bg = request_body("158", Some("4.10.3112.0"), true);
    assert_eq!(bg["request"]["apps"][0]["version"], "4.10.3112.0");
    assert_eq!(bg["request"]["apps"][0]["installsource"], "");
}

#[test]
fn an_offer_is_read_noupdate_is_none_an_error_is_an_error() {
    let ok = r#")]}'
{"response":{"protocol":"4.0","apps":[{"appid":"oimompecagnajdejgnnjijobebaeigek","status":"ok","updatecheck":{"nextversion":"4.10.3112.0","status":"ok","pipelines":[{"pipeline_id":"1:full","operations":[{"size":20423379,"type":"download","out":{"sha256":"6bf3"},"urls":[{"url":"http://edgedl.me.gvt1.com/a.crx3"},{"url":"https://edgedl.me.gvt1.com/a.crx3"}]},{"type":"crx3"}]}]}}]}}"#;
    assert_eq!(
        parse_offer(ok).unwrap(),
        Some(Offer {
            version: "4.10.3112.0".into(),
            urls: vec!["https://edgedl.me.gvt1.com/a.crx3".into()],
            sha256: "6bf3".into(),
            size: 20423379
        })
    );
    let none = r#")]}'
{"response":{"apps":[{"appid":"x","status":"ok","updatecheck":{"status":"noupdate"}}]}}"#;
    assert_eq!(parse_offer(none).unwrap(), None);
    let err = parse_offer(r#"{"response":{"apps":[{"status":"error-unknownApplication"}]}}"#).unwrap_err();
    assert!(err.to_string().contains("error-unknownApplication"), "{err}");
    assert!(parse_offer("<html>").is_err());
}

#[test]
fn the_id_is_the_keys_hash_as_a_to_p() {
    let hash = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10];
    assert_eq!(id_from_hash(&hash), "abcdefghijklmnopponmlkjihgfedcba");
}

#[test]
fn rsa_and_ecdsa_proofs_made_by_openssl_verify() {
    let all = fixture("signed_rsa_p256_p384");
    let v = verify_crx(&all, &Expect::default()).unwrap();
    assert_eq!(v.crx_id, fixture_str("signed_rsa_p256_p384_id"));
    assert_eq!(id_from_key(&v.developer_key), v.crx_id);
    assert_eq!(&all[v.archive_offset..v.archive_offset + 2], b"PK");
    let p256 = fixture("signed_p256");
    assert_eq!(verify_crx(&p256, &Expect::default()).unwrap().crx_id, fixture_str("signed_p256_id"));
}

#[test]
fn a_package_signed_for_another_id_is_refused() {
    let all = fixture("signed_rsa_p256_p384");
    let got = verify_crx(&all, &Expect { id: Some(WIDEVINE_ID), ..Default::default() }).unwrap_err();
    assert!(matches!(got, Reject::WrongId { .. }), "{got:?}");
}

#[test]
fn a_changed_byte_a_wrong_hash_or_size_or_no_developer_proof_is_refused() {
    // RSA first, and ECDSA alone.
    for name in ["signed_rsa_p256_p384", "signed_p256"] {
        let mut bent = fixture(name);
        let at = bent.len() - 3;
        bent[at] ^= 1;
        assert_eq!(verify_crx(&bent, &Expect::default()).unwrap_err(), Reject::BadSignature, "{name}");
    }
    let all = fixture("signed_rsa_p256_p384");
    let upper = verify_crx(&all, &Expect::default()).unwrap().sha256.to_uppercase();
    assert!(verify_crx(&all, &Expect { sha256: Some(&upper), size: Some(all.len() as u64), ..Default::default() }).is_ok());
    let zeros = "00".repeat(32);
    assert_eq!(verify_crx(&all, &Expect { sha256: Some(&zeros), ..Default::default() }).unwrap_err(), Reject::WrongHash);
    assert!(matches!(verify_crx(&all, &Expect { size: Some(1), ..Default::default() }), Err(Reject::WrongSize { .. })));
    assert_eq!(verify_crx(&fixture("no_developer_proof"), &Expect::default()).unwrap_err(), Reject::NoDeveloperProof);
    assert_eq!(verify_crx(b"PK\x03\x04 not a crx", &Expect::default()).unwrap_err(), Reject::NotCrx3);
}

#[test]
fn a_verified_package_is_unpacked_into_the_cache_for_its_version_only() {
    let root = temp("pkg");
    let cache = root.join("widevine-cdm");
    let all = fixture("signed_rsa_p256_p384");
    let v = verify_crx(&all, &Expect::default()).unwrap();
    assert!(install_package(&all, v.archive_offset, "4.10.9999.0", &cache).is_err());
    assert!(!cache.exists());
    install_package(&all, v.archive_offset, "4.10.3112.0", &cache).unwrap();
    assert_eq!(cdm_version_of(&cache).as_deref(), Some("4.10.3112.0"));
    assert!(cache.join("_platform_specific/linux_x64/libwidevinecdm.so").is_file());
    let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("widevine-cdm")]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_profile_gets_the_version_folder_and_the_hint_once() {
    let root = temp("layout");
    let cache = root.join("widevine-cdm");
    let udd = root.join("profiles").join("p1");
    std::fs::create_dir_all(&udd).unwrap();
    assert_eq!(install_widevine(&cache, &udd).unwrap(), None, "nothing cached yet");
    fake_cdm(&cache, "4.10.3112.0");
    // A flat layout an older build left.
    std::fs::create_dir_all(udd.join("WidevineCdm/_platform_specific")).unwrap();
    std::fs::write(udd.join("WidevineCdm/manifest.json"), r#"{"version":"4.10.1.0"}"#).unwrap();

    let versioned = install_widevine(&cache, &udd).unwrap().unwrap();
    assert_eq!(versioned, udd.join("WidevineCdm").join("4.10.3112.0"));
    assert_eq!(cdm_version_of(&versioned).as_deref(), Some("4.10.3112.0"));
    assert!(versioned.join("_platform_specific/mac_arm64/libwidevinecdm.dylib").is_file());
    assert!(!udd.join("WidevineCdm/manifest.json").exists(), "the flat layout is gone");
    let hint: Value =
        serde_json::from_str(&std::fs::read_to_string(udd.join("WidevineCdm").join(HINT_FILE)).unwrap()).unwrap();
    assert_eq!(hint, json!({ "Path": versioned.to_string_lossy() }));
    let mut names: Vec<String> = std::fs::read_dir(udd.join("WidevineCdm"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["4.10.3112.0".to_string(), HINT_FILE.to_string()]);
    assert_eq!(install_widevine(&cache, &udd).unwrap(), Some(versioned));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_newer_cdm_the_engine_fetched_is_taken_an_older_one_is_not() {
    let root = temp("harvest");
    let cache = root.join("widevine-cdm");
    let udd = root.join("p");
    fake_cdm(&cache, "4.10.3112.0");
    fake_cdm(&udd.join("WidevineCdm/4.10.2891.0"), "4.10.2891.0");
    assert_eq!(harvest_widevine(&cache, &udd).unwrap(), None);
    fake_cdm(&udd.join("WidevineCdm/4.11.0.0"), "4.11.0.0");
    assert_eq!(harvest_widevine(&cache, &udd).unwrap().as_deref(), Some("4.11.0.0"));
    assert_eq!(cdm_version_of(&cache).as_deref(), Some("4.11.0.0"));
    assert!(!std::fs::read_dir(&root).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains("incoming")));
    let _ = std::fs::remove_dir_all(&root);
}

type Calls = Arc<Mutex<Vec<(Option<String>, bool)>>>;

/// A stand-in for Google: notes each check and puts `answer` in the cache.
fn stand_in(calls: &Calls, answer: Option<&'static str>, delay: Duration) -> Fetcher {
    let calls = calls.clone();
    Arc::new(move |_proxy, _chrome, have, cache, background| {
        let calls = calls.clone();
        Box::pin(async move {
            calls.lock().unwrap().push((have, background));
            tokio::time::sleep(delay).await;
            if let Some(v) = answer {
                fake_cdm(&cache, v);
            }
            Ok(answer.map(String::from))
        })
    })
}

#[tokio::test]
async fn a_first_cdm_is_waited_for_a_fresh_one_is_not_checked_and_starts_at_once_check_once() {
    let root = temp("ensure");
    let cache = root.join("widevine-cdm");
    let calls: Calls = Arc::default();
    let fetch = stand_in(&calls, Some("4.10.3112.0"), Duration::from_millis(20));
    let first = Duration::from_secs(10);
    tokio::join!(
        ensure_with(&cache, "158.0.8127.36", None, fetch.clone(), first),
        ensure_with(&cache, "158.0.8127.36", None, fetch.clone(), first)
    );
    assert_eq!(*calls.lock().unwrap(), vec![(None, false)]);
    let stamp = root.join("widevine-cdm.checked");
    assert!(stamp.exists());
    ensure_with(&cache, "158.0.8127.36", None, fetch.clone(), first).await;
    assert_eq!(calls.lock().unwrap().len(), 1, "checked within 5 hours");
    // An old stamp: checked again, in the background.
    let old = SystemTime::now() - Duration::from_secs(6 * 3600);
    std::fs::File::options().write(true).open(&stamp).unwrap().set_modified(old).unwrap();
    ensure_with(&cache, "158.0.8127.36", None, stand_in(&calls, None, Duration::ZERO), first).await;
    for _ in 0..200 {
        if calls.lock().unwrap().len() > 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(calls.lock().unwrap()[1], (Some("4.10.3112.0".to_string()), true));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_first_check_that_takes_too_long_is_given_up() {
    let root = temp("slow");
    let cache = root.join("widevine-cdm");
    let calls: Calls = Arc::default();
    let slow = stand_in(&calls, Some("4.10.3112.0"), Duration::from_secs(30));
    let started = std::time::Instant::now();
    ensure_with(&cache, "158.0.8127.36", None, slow, Duration::from_millis(50)).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!root.join("widevine-cdm.checked").exists());
    assert!(cdm_version_of(&cache).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// By hand: `SHARDX_TEST_PROXY=socks5://user:pass@host:port cargo test widevine_live -- --nocapture`.
#[tokio::test]
async fn widevine_live_fetch_goes_through_the_proxy() {
    let Ok(url) = std::env::var("SHARDX_TEST_PROXY") else {
        eprintln!("skipped: reaches Google's servers; set SHARDX_TEST_PROXY to a proxy URL");
        return;
    };
    let base = crate::proxy::parse_proxy(&url).unwrap();
    let root = temp("live-proxy");
    // A proxy that is not there fails the fetch: nothing goes around it.
    let dead = ParsedProxy { host: "127.0.0.1".into(), port: 9, ..base.clone() };
    assert!(fetch_widevine(Some(&dead), "158.0.8127.36", None, &root.join("dead"), false).await.is_err());
    for scheme in [crate::proxy::ProxyScheme::Socks5, crate::proxy::ProxyScheme::Http] {
        let proxy = ParsedProxy { scheme, ..base.clone() };
        let cache = root.join(format!("{scheme:?}"));
        let v = fetch_widevine(Some(&proxy), "158.0.8127.36", None, &cache, false).await.unwrap().expect("an offer");
        assert_eq!(cdm_version_of(&cache).as_deref(), Some(v.as_str()));
        println!("widevine {v} through {scheme:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// By hand: `SHARDX_LIVE_WIDEVINE=1 cargo test widevine_live -- --nocapture`.
#[tokio::test]
async fn widevine_live_fetch_puts_a_signed_cdm_in_place() {
    if std::env::var_os("SHARDX_LIVE_WIDEVINE").is_none() {
        eprintln!("skipped: reaches Google's servers; set SHARDX_LIVE_WIDEVINE=1");
        return;
    }
    let root = temp("live");
    let cache = root.join("widevine-cdm");
    let v = fetch_widevine(None, "158.0.8127.36", None, &cache, false)
        .await
        .unwrap()
        .expect("an offer for a profile with none");
    assert_eq!(cdm_version_of(&cache).as_deref(), Some(v.as_str()));
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), out)
            } else {
                out.push(e.path().to_string_lossy().to_string())
            }
        }
    }
    let mut libs = Vec::new();
    walk(&cache.join("_platform_specific"), &mut libs);
    println!("widevine {v}: {libs:?}");
    assert!(libs.iter().any(|p| p.contains("widevinecdm")), "{libs:?}");
    let again = fetch_widevine(None, "158.0.8127.36", Some(&v), &cache, true).await.unwrap();
    assert_eq!(again, None, "the same version again is current");
    let _ = std::fs::remove_dir_all(&root);
}
