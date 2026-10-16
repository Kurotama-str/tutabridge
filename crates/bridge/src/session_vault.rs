//! Passphrase protection of `session.json` (file session store only).
//!
//! `session_encryption_method` `file` or `web` makes the bridge wait at start
//! for a passphrase, check it against the Argon2id hash in
//! `session_encryption_key`, and keep it in memory to encrypt and decrypt the
//! session file. The file holds `HEADER` + base64(salt ‖ nonce ‖ ciphertext);
//! the cipher key comes from Argon2id over the passphrase and that salt.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::{self, Config, SessionEncryptionMethod};

const HEADER: &str = "tutabridge-session-v1:";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;

static PASSPHRASE: Mutex<Option<String>> = Mutex::new(None);
static WRITE_BLOCKED: AtomicBool = AtomicBool::new(false);
static UNLOCK_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn is_encrypted(text: &str) -> bool {
    text.trim_start().starts_with(HEADER)
}

/// The passphrase entered at start, when encryption is active.
pub fn active_passphrase() -> Option<String> {
    PASSPHRASE.lock().unwrap().clone()
}

/// An encrypted session file the bridge cannot open must never be replaced by
/// a plaintext one: while this is set, nothing is written to `session.json`.
pub fn block_writes() {
    WRITE_BLOCKED.store(true, Ordering::SeqCst);
}

pub fn writes_blocked() -> bool {
    WRITE_BLOCKED.load(Ordering::SeqCst)
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Key derivation failed: {e}"))?;
    Ok(key)
}

pub fn encrypt(passphrase: &str, plaintext: &[u8]) -> Result<String, String> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce);
    let key = derive_key(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new(&key.into());
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .map_err(|_| "Encryption failed".to_string())?;
    let mut blob = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    Ok(format!(
        "{HEADER}{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, blob)
    ))
}

pub fn decrypt(passphrase: &str, text: &str) -> Result<Vec<u8>, String> {
    let body = text
        .trim()
        .strip_prefix(HEADER)
        .ok_or("Not an encrypted session file")?;
    let blob = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body.trim())
        .map_err(|_| "Encrypted session file is corrupt")?;
    if blob.len() < SALT_LEN + NONCE_LEN {
        return Err("Encrypted session file is truncated".into());
    }
    let (salt, rest) = blob.split_at(SALT_LEN);
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let key = derive_key(passphrase, salt)?;
    XChaCha20Poly1305::new(&key.into())
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .map_err(|_| "Wrong passphrase or corrupt session file".to_string())
}

/// The active method and passphrase hash, or `None` when the session file is
/// stored unencrypted: method `none`, an empty key, or a key that is not an
/// Argon2id hash (each of the last two is logged as a warning).
fn active_method(cfg: &Config) -> Option<(SessionEncryptionMethod, String)> {
    if cfg.session_encryption_method == SessionEncryptionMethod::None {
        return None;
    }
    let key = cfg.session_encryption_key.trim();
    if key.is_empty() {
        log::warn!(
            "session_encryption_method is set but session_encryption_key is empty; \
             the session is stored UNENCRYPTED"
        );
        return None;
    }
    let valid = crate::bridge_auth::is_hashed(key)
        && argon2::password_hash::PasswordHash::new(key).is_ok();
    if !valid {
        log::warn!(
            "session_encryption_key is not a valid Argon2id hash (make one with \
             `tutabridge hash-password`); the session is stored UNENCRYPTED"
        );
        return None;
    }
    Some((cfg.session_encryption_method, key.to_string()))
}

/// Wait until the passphrase is known when encryption is active. Idempotent;
/// returns at once when encryption is off or the session is in the keychain.
pub async fn ensure_unlocked(cfg: &Config) -> Result<(), String> {
    if !config::session_store_is_file() {
        if cfg.session_encryption_method != SessionEncryptionMethod::None {
            log::warn!(
                "session_encryption_method is ignored: the session is kept in the OS keychain"
            );
        }
        return Ok(());
    }
    let _gate = UNLOCK_GATE.lock().await;
    if PASSPHRASE.lock().unwrap().is_some() {
        return Ok(());
    }
    let Some((method, hash)) = active_method(cfg) else {
        let existing = std::fs::read_to_string(config::session_file_path()).unwrap_or_default();
        if is_encrypted(&existing) {
            block_writes();
            log::error!(
                "{} is encrypted but session encryption is not configured; it is left \
                 untouched and the saved session is not used. Fix session_encryption_* in \
                 config.toml, or delete the file to log in again.",
                config::session_file_path().display()
            );
        }
        return Ok(());
    };
    let passphrase = match method {
        SessionEncryptionMethod::File => wait_for_file(cfg, &hash).await?,
        _ => wait_for_web(cfg, &hash).await?,
    };
    *PASSPHRASE.lock().unwrap() = Some(passphrase);
    log::info!("Session unlocked");
    Ok(())
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((2 * u64::from(failures)).min(30))
}

async fn wait_for_file(cfg: &Config, hash: &str) -> Result<String, String> {
    let path = cfg
        .session_unlock_file
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(config::default_unlock_file_path);
    let ensure_empty = || -> Result<(), String> {
        if let Some(dir) = path.parent() {
            crate::fs_private::create_dir_all_private(dir)
                .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
        crate::fs_private::write_private(&path, "")
            .map_err(|e| format!("Cannot write {}: {e}", path.display()))
    };
    if !path.exists() {
        ensure_empty()?;
    }
    log::info!(
        "Session is encrypted: write the passphrase to {}",
        path.display()
    );
    let mut failures = 0u32;
    loop {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let candidate = text.trim_end_matches(['\r', '\n']);
                if !candidate.is_empty() {
                    if crate::bridge_auth::verify(candidate, hash) {
                        if let Err(e) = std::fs::remove_file(&path) {
                            log::warn!("Could not delete {}: {e}", path.display());
                        }
                        return Ok(candidate.to_string());
                    }
                    failures += 1;
                    log::warn!("Wrong session passphrase in {}", path.display());
                    ensure_empty()?;
                    tokio::time::sleep(backoff(failures)).await;
                    continue;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ensure_empty()?,
            Err(e) => log::warn!("Cannot read {}: {e}", path.display()),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_web(cfg: &Config, hash: &str) -> Result<String, String> {
    let ip = cfg.bind_ip()?;
    let listener =
        crate::net::bind_service("Session unlock", ip, cfg.session_unlock_port).await?;
    let acceptor = crate::tls::load_or_create_tls_acceptor()
        .map_err(|e| format!("TLS setup for the unlock page failed: {e}"))?;
    log::info!(
        "Session is encrypted: open https://{ip}:{} to enter the passphrase",
        cfg.session_unlock_port
    );
    let mut failures = 0u32;
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                log::warn!("Unlock page accept failed: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let attempt = tokio::time::timeout(
            Duration::from_secs(15),
            serve_unlock_request(stream, &acceptor, hash),
        )
        .await;
        match attempt {
            Ok(Some(Ok(passphrase))) => return Ok(passphrase),
            Ok(Some(Err(()))) => {
                failures += 1;
                log::warn!("Wrong session passphrase entered on the unlock page");
                tokio::time::sleep(backoff(failures)).await;
            }
            _ => {}
        }
    }
}

const PAGE_HEAD: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>TutaBridge unlock</title></head><body style=\"font-family:sans-serif;max-width:28em;margin:4em auto;padding:0 1em\">\
<h1>TutaBridge</h1>";

fn form_page(error: bool) -> String {
    let note = if error {
        "<p style=\"color:#b00\">Wrong passphrase.</p>"
    } else {
        "<p>The saved session is encrypted. Enter the passphrase to start the bridge.</p>"
    };
    format!(
        "{PAGE_HEAD}{note}<form method=\"post\" action=\"/\">\
<input type=\"password\" name=\"passphrase\" autofocus autocomplete=\"off\" style=\"width:100%;padding:.5em\">\
<p><button type=\"submit\" style=\"padding:.5em 1.5em\">Unlock</button></p></form></body></html>"
    )
}

async fn respond<S: AsyncWriteExt + Unpin>(stream: &mut S, status: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(msg.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// One HTTPS request. `Some(Ok(pass))`: correct passphrase. `Some(Err(()))`:
/// wrong one. `None`: anything else (page view, bad request, I/O error).
async fn serve_unlock_request(
    stream: tokio::net::TcpStream,
    acceptor: &tokio_rustls::TlsAcceptor,
    hash: &str,
) -> Option<Result<String, ()>> {
    let mut tls = acceptor.accept(stream).await.ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 8192 {
            return None;
        }
        let n = tls.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let request_line = head.lines().next()?;
    if !request_line.starts_with("POST ") {
        respond(&mut tls, "200 OK", &form_page(false)).await;
        return None;
    }
    let content_length: usize = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    if content_length > 4096 {
        respond(&mut tls, "413 Payload Too Large", "Too large").await;
        return None;
    }
    while buf.len() < head_end + content_length {
        let n = tls.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end..head_end + content_length]).to_string();
    let passphrase = form_field(&body, "passphrase").unwrap_or_default();
    if !passphrase.is_empty() && crate::bridge_auth::verify(&passphrase, hash) {
        let done = format!("{PAGE_HEAD}<p>Unlocked. The bridge is starting.</p></body></html>");
        respond(&mut tls, "200 OK", &done).await;
        return Some(Ok(passphrase));
    }
    respond(&mut tls, "401 Unauthorized", &form_page(true)).await;
    Some(Err(()))
}

fn form_field(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decode(value))
    })
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => match raw.get(i + 1..i + 3).map(|h| u8::from_str_radix(h, 16)) {
                Some(Ok(b)) => {
                    out.push(b);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_passphrase() {
        let sealed = encrypt("pass", b"{\"a\":1}").unwrap();
        assert!(is_encrypted(&sealed));
        assert_eq!(decrypt("pass", &sealed).unwrap(), b"{\"a\":1}");
        assert!(decrypt("other", &sealed).is_err());
        assert!(!sealed.contains("\"a\""));
    }

    #[test]
    fn plaintext_is_not_detected_as_encrypted() {
        assert!(!is_encrypted("{\"login\":\"x\"}"));
    }

    #[test]
    fn form_decoding() {
        assert_eq!(
            form_field("passphrase=ab%2Bc+d&x=1", "passphrase").unwrap(),
            "ab+c d"
        );
        assert_eq!(form_field("passphrase=abc%", "passphrase").unwrap(), "abc%");
        assert!(form_field("x=1", "passphrase").is_none());
    }

    #[test]
    fn invalid_configuration_falls_back_to_unencrypted() {
        let mut cfg = Config {
            session_encryption_method: SessionEncryptionMethod::File,
            ..Config::default()
        };
        assert!(active_method(&cfg).is_none(), "empty key");
        cfg.session_encryption_key = "not-a-hash".into();
        assert!(active_method(&cfg).is_none(), "malformed key");
        cfg.session_encryption_key = crate::bridge_auth::hash("pw").unwrap();
        assert!(active_method(&cfg).is_some());
        cfg.session_encryption_method = SessionEncryptionMethod::None;
        assert!(active_method(&cfg).is_none());
    }
}
