//! `session.json` in the config directory, for installs with no keychain
//! (`TUTABRIDGE_SESSION_STORE=file`). When session encryption is configured,
//! `session_vault` encrypts it with the passphrase entered at start.

use std::path::PathBuf;

use super::{codec, SecretBytes};
use crate::{config, fs_private, session_vault};

pub(super) struct FileStore {
    email: String,
    path: PathBuf,
}

impl FileStore {
    pub(super) fn new(email: String) -> Self {
        Self {
            email,
            path: config::session_file_path(),
        }
    }

    pub(super) fn entry(&self) -> String {
        self.path.display().to_string()
    }

    pub(super) fn read(&self) -> Result<Option<SecretBytes>, String> {
        fs_private::tighten(&self.path);
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => zeroize::Zeroizing::new(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", self.path.display())),
        };
        let json = if session_vault::is_encrypted(&text) {
            let Some(passphrase) = session_vault::active_passphrase() else {
                session_vault::block_writes();
                return Err(format!(
                    "{} is encrypted but no passphrase is available",
                    self.path.display()
                ));
            };
            zeroize::Zeroizing::new(session_vault::decrypt(&passphrase, &text).map_err(|e| {
                session_vault::block_writes();
                format!("cannot decrypt {} ({e}); it is left untouched", self.path.display())
            })?)
        } else {
            // A plaintext file with encryption active: encrypt it now.
            if session_vault::active_passphrase().is_some() {
                if let Err(e) = self.write(&text) {
                    log::warn!("Could not encrypt the existing session file: {e}");
                }
            }
            zeroize::Zeroizing::new(text.as_bytes().to_vec())
        };
        // The file is not keyed by address the way a keychain entry is, so a
        // session saved for another account must not be resumed.
        match codec::decode(&json) {
            Some(credentials) if !credentials.login.eq_ignore_ascii_case(&self.email) => Ok(None),
            _ => Ok(Some(json)),
        }
    }

    /// Nothing is written while an encrypted file the bridge could not open is
    /// protected, so it is never replaced by a plaintext one.
    pub(super) fn write(&self, json: &str) -> Result<(), String> {
        if session_vault::writes_blocked() {
            return Err(format!(
                "{} is encrypted and could not be opened",
                self.path.display()
            ));
        }
        let contents = match session_vault::active_passphrase() {
            Some(passphrase) => session_vault::encrypt(&passphrase, json.as_bytes())
                .map_err(|e| format!("failed to encrypt the session: {e}"))?,
            None => json.to_owned(),
        };
        self.path
            .parent()
            .map_or(Ok(()), fs_private::create_dir_all_private)
            .and_then(|_| fs_private::write_private(&self.path, contents))
            .map_err(|e| e.to_string())
    }

    pub(super) fn delete(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
