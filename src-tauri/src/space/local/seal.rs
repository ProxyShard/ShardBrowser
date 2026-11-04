//! Password-protected profiles (#127): the keys, and the format of what they seal.
//! Argon2id turns the password into a key that wraps a random data key; the data key seals the
//! config as one block and the browser data as a stream of chunks, each bound to the profile,
//! its place and whether it ends the stream.

use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// What `Lock` and the sealed streams are written in; a newer one is refused, not half-read.
const FORMAT: u32 = 1;
/// Plaintext per chunk of a sealed stream.
pub const CHUNK: usize = 512 * 1024;
const MAGIC: &[u8; 8] = b"SBSEAL1\0";
/// Shown for a wrong password and for anything that does not open, alike.
pub const WRONG_PASSWORD: &str = "wrong password";

/// Argon2id cost. About half a second on a current laptop: once per start, not per request.
#[cfg(not(test))]
const M_KIB: u32 = 64 * 1024;
#[cfg(test)]
const M_KIB: u32 = 64;
#[cfg(not(test))]
const T_COST: u32 = 3;
#[cfg(test)]
const T_COST: u32 = 1;
const P_COST: u32 = 1;

/// What the profile file keeps in the open to unlock it: the KDF and the wrapped data key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Lock {
    pub v: u32,
    pub kdf: Kdf,
    /// base64(nonce || AES-256-GCM(data key)).
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Kdf {
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    /// base64, 16 bytes.
    pub salt: String,
}

/// The data key, wiped from memory when dropped.
pub struct DataKey(Zeroizing<[u8; 32]>);

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    SystemRandom::new().fill(&mut b).map_err(|_| anyhow!("no system randomness"))?;
    Ok(b)
}

fn kek(password: &str, kdf: &Kdf) -> Result<Zeroizing<[u8; 32]>> {
    if kdf.alg != "argon2id" {
        bail!("unknown key derivation {}", kdf.alg);
    }
    let salt = B64.decode(&kdf.salt).context("salt")?;
    let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| anyhow!("{e}"))?;
    let mut out = Zeroizing::new([0u8; 32]);
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password.as_bytes(), &salt, out.as_mut())
        .map_err(|e| anyhow!("{e}"))?;
    Ok(out)
}

fn aead(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("32-byte key"))
}

/// Seal `plain` under `key`, bound to `aad`: base64(nonce || ciphertext || tag).
fn seal_with(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Result<String> {
    let nonce: [u8; NONCE_LEN] = random()?;
    let mut buf = plain.to_vec();
    aead(key)
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad), &mut buf)
        .map_err(|_| anyhow!("seal failed"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&buf);
    Ok(B64.encode(out))
}

fn open_with(key: &[u8; 32], aad: &[u8], sealed: &str) -> Result<Vec<u8>> {
    let raw = B64.decode(sealed).map_err(|_| anyhow!(WRONG_PASSWORD))?;
    if raw.len() < NONCE_LEN {
        bail!(WRONG_PASSWORD);
    }
    let (nonce, body) = raw.split_at(NONCE_LEN);
    let mut buf = body.to_vec();
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| anyhow!(WRONG_PASSWORD))?;
    let plain = aead(key)
        .open_in_place(nonce, Aad::from(aad), &mut buf)
        .map_err(|_| anyhow!(WRONG_PASSWORD))?;
    Ok(plain.to_vec())
}

fn key_aad(id: &str) -> Vec<u8> {
    format!("sbseal1|key|{id}").into_bytes()
}

/// A fresh data key for profile `id`, and the lock that opens it with `password`.
pub fn new_lock(id: &str, password: &str) -> Result<(Lock, DataKey)> {
    let key = DataKey(Zeroizing::new(random()?));
    let lock = wrap(id, &key, password)?;
    Ok((lock, key))
}

/// The same data key under a new password: nothing else is sealed again.
pub fn wrap(id: &str, key: &DataKey, password: &str) -> Result<Lock> {
    let kdf = Kdf { alg: "argon2id".into(), m_kib: M_KIB, t: T_COST, p: P_COST, salt: B64.encode(random::<16>()?) };
    let wrapping = kek(password, &kdf)?;
    Ok(Lock { v: FORMAT, kdf, key: seal_with(&wrapping, &key_aad(id), key.0.as_ref())? })
}

/// The data key of profile `id`; `WRONG_PASSWORD` when `password` does not open it.
pub fn unlock(id: &str, lock: &Lock, password: &str) -> Result<DataKey> {
    if lock.v > FORMAT {
        bail!("this profile was protected by a newer launcher");
    }
    let wrapping = kek(password, &lock.kdf)?;
    let raw = Zeroizing::new(open_with(&wrapping, &key_aad(id), &lock.key)?);
    let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| anyhow!(WRONG_PASSWORD))?;
    Ok(DataKey(Zeroizing::new(bytes)))
}

/// A sealed block of profile `id`, named by `what` so one block cannot stand in for another.
pub fn seal_blob(id: &str, key: &DataKey, what: &str, plain: &[u8]) -> Result<String> {
    seal_with(&key.0, format!("sbseal1|{what}|{id}").as_bytes(), plain)
}

pub fn open_blob(id: &str, key: &DataKey, what: &str, sealed: &str) -> Result<Vec<u8>> {
    open_with(&key.0, format!("sbseal1|{what}|{id}").as_bytes(), sealed)
}

fn chunk_aad(id: &str, seq: u64, last: bool) -> Vec<u8> {
    let mut a = format!("sbseal1|data|{id}|").into_bytes();
    a.extend_from_slice(&seq.to_be_bytes());
    a.push(last as u8);
    a
}

fn chunk_nonce(prefix: &[u8; 4], seq: u64) -> Nonce {
    let mut n = [0u8; NONCE_LEN];
    n[..4].copy_from_slice(prefix);
    n[4..].copy_from_slice(&seq.to_be_bytes());
    Nonce::assume_unique_for_key(n)
}

/// Seals what is written to it, chunk by chunk; `finish` writes the closing chunk.
pub struct SealWriter<W: Write> {
    out: W,
    key: LessSafeKey,
    id: String,
    prefix: [u8; 4],
    seq: u64,
    buf: Vec<u8>,
}

impl<W: Write> SealWriter<W> {
    pub fn new(mut out: W, id: &str, key: &DataKey) -> Result<Self> {
        let prefix: [u8; 4] = random()?;
        out.write_all(MAGIC)?;
        out.write_all(&prefix)?;
        Ok(SealWriter { out, key: aead(&key.0), id: id.to_string(), prefix, seq: 0, buf: Vec::with_capacity(CHUNK) })
    }

    fn emit(&mut self, last: bool) -> io::Result<()> {
        let mut chunk = std::mem::take(&mut self.buf);
        self.key
            .seal_in_place_append_tag(chunk_nonce(&self.prefix, self.seq), Aad::from(chunk_aad(&self.id, self.seq, last)), &mut chunk)
            .map_err(|_| io::Error::other("seal failed"))?;
        self.out.write_all(&[last as u8])?;
        self.out.write_all(&(chunk.len() as u32).to_be_bytes())?;
        self.out.write_all(&chunk)?;
        self.seq += 1;
        self.buf = Vec::with_capacity(CHUNK);
        Ok(())
    }

    /// The closing chunk; without it a reader takes the stream for cut off.
    pub fn finish(mut self) -> io::Result<W> {
        self.emit(true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let take = data.len().min(CHUNK - self.buf.len());
        self.buf.extend_from_slice(&data[..take]);
        if self.buf.len() == CHUNK {
            self.emit(false)?;
        }
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Opens a stream `SealWriter` wrote; a swapped, reordered, altered or cut-off chunk is an error.
pub struct OpenReader<R: Read> {
    src: R,
    key: LessSafeKey,
    id: String,
    prefix: [u8; 4],
    seq: u64,
    plain: Vec<u8>,
    at: usize,
    ended: bool,
}

impl<R: Read> OpenReader<R> {
    pub fn new(mut src: R, id: &str, key: &DataKey) -> Result<Self> {
        let mut head = [0u8; 12];
        src.read_exact(&mut head).map_err(|_| anyhow!("not a sealed profile archive"))?;
        if &head[..8] != MAGIC {
            bail!("not a sealed profile archive");
        }
        let prefix = head[8..].try_into().expect("4 bytes");
        Ok(OpenReader { src, key: aead(&key.0), id: id.to_string(), prefix, seq: 0, plain: Vec::new(), at: 0, ended: false })
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let mut frame = [0u8; 5];
        self.src.read_exact(&mut frame).map_err(|_| io::Error::other("sealed archive is cut off"))?;
        let last = match frame[0] {
            0 => false,
            1 => true,
            _ => return Err(io::Error::other(WRONG_PASSWORD)),
        };
        let len = u32::from_be_bytes(frame[1..].try_into().expect("4 bytes")) as usize;
        if len > CHUNK + AES_256_GCM.tag_len() {
            return Err(io::Error::other(WRONG_PASSWORD));
        }
        let mut chunk = vec![0u8; len];
        self.src.read_exact(&mut chunk).map_err(|_| io::Error::other("sealed archive is cut off"))?;
        let plain = self
            .key
            .open_in_place(chunk_nonce(&self.prefix, self.seq), Aad::from(chunk_aad(&self.id, self.seq, last)), &mut chunk)
            .map_err(|_| io::Error::other(WRONG_PASSWORD))?;
        self.plain = plain.to_vec();
        self.at = 0;
        self.seq += 1;
        self.ended = last;
        Ok(())
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            if self.ended {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = out.len().min(self.plain.len() - self.at);
        out[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Never sealed: caches the browser makes again, and what a start writes afresh.
const SKIP: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "GrShaderCache",
    "GraphiteDawnCache",
    "ShaderCache",
    "GPUPersistentCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "Crashpad",
    "WidevineCdm",
    "BrowserMetrics",
    "BrowserMetrics-spare.pma",
    "SingletonLock",
    "SingletonSocket",
    "SingletonCookie",
    "DevToolsActivePort",
    "fingerprint.json",
];

fn add_dir<W: Write>(tar: &mut tar::Builder<W>, root: &Path, dir: &Path) -> Result<()> {
    for e in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let e = e?;
        let name = e.file_name();
        if SKIP.iter().any(|s| name == *s) {
            continue;
        }
        let path = e.path();
        // symlink_metadata: a link is never followed out of the profile.
        let meta = fs::symlink_metadata(&path)?;
        let rel = path.strip_prefix(root).expect("under root");
        if meta.is_dir() {
            tar.append_dir(rel, &path)?;
            add_dir(tar, root, &path)?;
        } else if meta.is_file() {
            tar.append_path_with_name(&path, rel)?;
        }
    }
    Ok(())
}

/// Seal the browser data in `dir` to `out`: written beside it and renamed, so a crash leaves the old one.
pub fn seal_dir(dir: &Path, out: &Path, id: &str, key: &DataKey) -> Result<()> {
    let tmp = out.with_extension("sealing");
    let file = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut tar = tar::Builder::new(SealWriter::new(BufWriter::new(file), id, key)?);
    tar.follow_symlinks(false);
    add_dir(&mut tar, dir, dir)?;
    let file = tar.into_inner()?.finish()?.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, out).with_context(|| format!("place {}", out.display()))?;
    Ok(())
}

/// Open what `seal_dir` sealed into `dir`; an entry pointing outside it is refused.
pub fn open_dir(sealed: &Path, dir: &Path, id: &str, key: &DataKey) -> Result<()> {
    let file = fs::File::open(sealed).with_context(|| format!("open {}", sealed.display()))?;
    fs::create_dir_all(dir)?;
    let mut tar = tar::Archive::new(OpenReader::new(BufReader::new(file), id, key)?);
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.unpack_in(dir)? {
            bail!("sealed archive names a path outside the profile");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheap(id: &str, password: &str) -> (Lock, DataKey) {
        // The real cost makes a test run slow; the format is the same.
        let key = DataKey(Zeroizing::new(random().unwrap()));
        let kdf = Kdf { alg: "argon2id".into(), m_kib: 64, t: 1, p: 1, salt: B64.encode(random::<16>().unwrap()) };
        let wrapping = kek(password, &kdf).unwrap();
        let lock = Lock { v: FORMAT, kdf, key: seal_with(&wrapping, &key_aad(id), key.0.as_ref()).unwrap() };
        (lock, key)
    }

    fn sealed_stream(id: &str, key: &DataKey, data: &[u8]) -> Vec<u8> {
        let mut w = SealWriter::new(Vec::new(), id, key).unwrap();
        w.write_all(data).unwrap();
        w.finish().unwrap()
    }

    fn open_stream(id: &str, key: &DataKey, sealed: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        OpenReader::new(sealed, id, key).map_err(io::Error::other)?.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn a_profile_folder_comes_back_without_its_caches_and_links() {
        let (_, key) = cheap("p1", "pw");
        let root = std::env::temp_dir().join(format!("sbseal-{}", std::process::id()));
        let (src, dst, out) = (root.join("src"), root.join("dst"), root.join("p1.data"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(src.join("Default/Local Storage/leveldb")).unwrap();
        fs::create_dir_all(src.join("Default/Cache/Cache_Data")).unwrap();
        fs::create_dir_all(src.join("Default/Empty")).unwrap();
        fs::write(src.join("Local State"), b"{}").unwrap();
        fs::write(src.join("Default/Cookies"), vec![9u8; 3 * CHUNK]).unwrap();
        fs::write(src.join("Default/Local Storage/leveldb/000003.log"), b"site").unwrap();
        fs::write(src.join("Default/Cache/Cache_Data/f_000001"), b"cache").unwrap();
        fs::write(src.join("fingerprint.json"), b"{}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", src.join("SingletonSocket")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hosts", src.join("Default/a-link")).unwrap();

        seal_dir(&src, &out, "p1", &key).unwrap();
        assert!(!out.with_extension("sealing").exists());
        open_dir(&out, &dst, "p1", &key).unwrap();

        assert_eq!(fs::read(dst.join("Default/Cookies")).unwrap(), vec![9u8; 3 * CHUNK]);
        assert_eq!(fs::read(dst.join("Default/Local Storage/leveldb/000003.log")).unwrap(), b"site");
        assert!(dst.join("Default/Empty").is_dir());
        assert!(!dst.join("Default/Cache").exists());
        assert!(!dst.join("fingerprint.json").exists());
        assert!(!dst.join("SingletonSocket").exists());
        assert!(!dst.join("Default/a-link").exists());
        // The wrong key opens nothing and writes nothing.
        let (_, other) = cheap("p1", "pw");
        assert!(open_dir(&out, &root.join("dst2"), "p1", &other).is_err());
        assert!(fs::read_dir(root.join("dst2")).map(|d| d.count()).unwrap_or(0) == 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_right_password_opens_the_key_and_a_wrong_one_does_not() {
        let (lock, key) = cheap("p1", "correct horse");
        let again = unlock("p1", &lock, "correct horse").unwrap();
        assert_eq!(again.0.as_ref(), key.0.as_ref());
        assert_eq!(unlock("p1", &lock, "wrong").err().unwrap().to_string(), WRONG_PASSWORD);
        // Bound to its profile: the same lock does not open under another id.
        assert!(unlock("p2", &lock, "correct horse").is_err());
    }

    #[test]
    fn a_new_password_wraps_the_same_key() {
        let (_, key) = cheap("p1", "old");
        let lock = wrap("p1", &key, "new").unwrap();
        assert_eq!(lock.kdf.m_kib, M_KIB);
        assert_eq!(unlock("p1", &lock, "new").unwrap().0.as_ref(), key.0.as_ref());
    }

    #[test]
    fn a_blob_opens_only_as_what_it_was_sealed_as() {
        let (_, key) = cheap("p1", "pw");
        let s = seal_blob("p1", &key, "config", b"{\"a\":1}").unwrap();
        assert_eq!(open_blob("p1", &key, "config", &s).unwrap(), b"{\"a\":1}");
        assert!(open_blob("p1", &key, "proxy", &s).is_err());
        assert!(open_blob("p2", &key, "config", &s).is_err());
    }

    #[test]
    fn a_stream_round_trips_across_chunks() {
        let (_, key) = cheap("p1", "pw");
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let sealed = sealed_stream("p1", &key, &data);
            assert_eq!(open_stream("p1", &key, &sealed).unwrap(), data, "{len}");
        }
    }

    #[test]
    fn a_stream_cut_off_altered_or_moved_does_not_open() {
        let (_, key) = cheap("p1", "pw");
        let data = vec![7u8; 2 * CHUNK + 10];
        let sealed = sealed_stream("p1", &key, &data);
        // Cut at a chunk boundary: every chunk left is intact, but none says it is the last.
        let first = 12 + 5 + CHUNK + 16;
        assert!(open_stream("p1", &key, &sealed[..first]).is_err());
        let mut flipped = sealed.clone();
        flipped[first + 40] ^= 1;
        assert!(open_stream("p1", &key, &flipped).is_err());
        assert!(open_stream("p2", &key, &sealed).is_err());
        let (_, other) = cheap("p1", "pw");
        assert!(open_stream("p1", &other, &sealed).is_err());
    }
}
