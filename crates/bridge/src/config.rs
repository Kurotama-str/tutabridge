use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub email: String,
    pub imap_port: u16,
    pub smtp_port: u16,
    /// Address for the mail listeners only. MCP always remains on loopback.
    /// Kept outside EditableSettings so desktop saves preserve manual values.
    /// `bind_address` is the older spelling and is still read.
    /// The `TUTABRIDGE_BIND_ADDRESS` environment variable overrides it.
    #[serde(
        default = "default_bind_addr",
        alias = "bind_address",
        skip_serializing_if = "is_default_bind_addr"
    )]
    pub bind_addr: IpAddr,
    #[serde(default = "default_api_url")]
    pub api_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_password: Option<String>,
    #[serde(default = "default_sync_limit")]
    pub sync_limit: usize,
    /// Read-only MCP server permission. `Disabled` (the default) means the
    /// server isn't started at all.
    #[serde(default)]
    pub mcp_permission: McpPermission,
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
    /// The GUI looks for a new version on GitHub and installs it. Off means
    /// it never contacts GitHub on its own. The CLI never updates itself.
    #[serde(default = "default_auto_update")]
    pub auto_update: bool,
    /// How `session.json` is protected when the session is kept in a file
    /// (`TUTABRIDGE_SESSION_STORE=file`). `file` and `web` encrypt it with a
    /// passphrase that is supplied at every start; they need
    /// `session_encryption_key`. The OS keychain is never affected.
    #[serde(default)]
    pub session_encryption_method: SessionEncryptionMethod,
    /// Argon2id hash (`$argon2id$...`, see `tutabridge hash-password`) of the
    /// session passphrase. Only used to check the passphrase that is entered.
    #[serde(default)]
    pub session_encryption_key: String,
    /// Method `file`: the file the passphrase is written to. Defaults to
    /// `unlock` in the config directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_unlock_file: Option<String>,
    /// Method `web`: port of the unlock page (TLS, on the bind address).
    #[serde(default = "default_unlock_port")]
    pub session_unlock_port: u16,

    /// Keep the login session in pass (password-store) as
    /// `<pass_path>/<email>` instead of the OS keyring. Unset means the
    /// keyring on supported desktop platforms; Android requires this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionEncryptionMethod {
    /// `session.json` is stored unencrypted.
    #[default]
    None,
    /// The passphrase is written to the unlock file.
    File,
    /// The passphrase is entered on a web page.
    Web,
}

/// The fields the settings form owns. Hidden configuration must come from
/// the current file, never from a partial or stale frontend snapshot.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditableSettings {
    pub email: String,
    pub imap_port: u16,
    pub smtp_port: u16,
    pub api_url: String,
    pub sync_limit: usize,
    pub mcp_permission: McpPermission,
    pub mcp_port: u16,
    pub auto_update: bool,
}

/// What a connected LLM may read over the MCP server. Strictly read-only — the
/// server never exposes a tool that sends, moves, deletes or otherwise mutates
/// mail. The tiers differ only in how much they let a client *read*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpPermission {
    /// Server off.
    #[default]
    Disabled,
    /// Folder list, metadata search (subject / sender / date), and message
    /// headers only — never body content.
    Metadata,
    /// Everything `Metadata` allows, plus full-text body search and message
    /// body content.
    Full,
}

impl McpPermission {
    /// May tools read message body content (and search bodies)?
    pub fn allows_body(self) -> bool {
        matches!(self, McpPermission::Full)
    }

    /// Is the MCP server enabled at all?
    pub fn is_enabled(self) -> bool {
        !matches!(self, McpPermission::Disabled)
    }
}

/// The `TUTABRIDGE_BIND_ADDRESS` override: `None` when unset or blank, an
/// error message when it is not a valid IP address.
fn bind_override(raw: Option<&str>) -> Result<Option<IpAddr>, String> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| format!("TUTABRIDGE_BIND_ADDRESS \"{value}\" is not a valid IP address")),
    }
}

fn default_sync_limit() -> usize {
    500
}

fn default_bind_addr() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

fn is_default_bind_addr(address: &IpAddr) -> bool {
    *address == default_bind_addr()
}

fn default_mcp_port() -> u16 {
    1944
}

fn default_api_url() -> String {
    "https://app.tuta.com".to_string()
}

fn default_unlock_port() -> u16 {
    8080
}

fn default_auto_update() -> bool {
    true
}

impl Config {
    /// The address the IMAP and SMTP servers bind to: the
    /// `TUTABRIDGE_BIND_ADDRESS` environment variable when it holds a valid
    /// IP address, else `bind_addr`.
    pub fn bind_ip(&self) -> IpAddr {
        static WARN_INVALID: std::sync::Once = std::sync::Once::new();
        let raw = std::env::var("TUTABRIDGE_BIND_ADDRESS").ok();
        match bind_override(raw.as_deref()) {
            Ok(Some(ip)) => ip,
            Ok(None) => self.bind_addr,
            Err(message) => {
                WARN_INVALID.call_once(|| log::warn!("{message}; using {}", self.bind_addr));
                self.bind_addr
            }
        }
    }

    /// Security notices for startup, as `(level, message)`, shared by the CLI
    /// and the desktop log. Call it once `bridge_password` has been generated.
    pub fn security_notices(&self) -> Vec<(log::Level, String)> {
        self.notices_for(!self.bind_ip().is_loopback(), session_store_is_file())
    }

    fn notices_for(&self, exposed: bool, file_store: bool) -> Vec<(log::Level, String)> {
        let mut notices = Vec::new();
        if exposed {
            notices.push((
                log::Level::Warn,
                "Listening beyond this machine: the bridge password is the only protection, \
                 and exposing these ports is at your own risk. In a container this address is \
                 expected; what matters is where the ports are published."
                    .to_owned(),
            ));
            let plaintext = self
                .bridge_password
                .as_deref()
                .is_some_and(|password| !password.is_empty() && !crate::bridge_auth::is_hashed(password));
            if plaintext {
                notices.push((
                    log::Level::Warn,
                    "bridge_password is stored in plaintext in config.toml. Run \
                     `tutabridge hash-password` and put the hash there; mail clients keep \
                     using the plain password."
                        .to_owned(),
                ));
            }
        }
        if file_store && self.session_encryption_method == SessionEncryptionMethod::None {
            notices.push((
                if exposed { log::Level::Warn } else { log::Level::Info },
                "The saved session in session.json is not encrypted; set \
                 session_encryption_method to `file` or `web` to protect it at rest."
                    .to_owned(),
            ));
        }
        notices
    }

    pub fn imap_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_ip(), self.imap_port)
    }

    pub fn smtp_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_ip(), self.smtp_port)
    }

    /// The listening ports must be usable and distinct. Two services on one
    /// port make the second bind fail at start; port 0 makes the system pick
    /// a random port no mail client would ever find. The MCP port only counts
    /// when the MCP server is enabled.
    pub fn validate_ports(&self) -> Result<(), String> {
        let mut ports = vec![("IMAP", self.imap_port), ("SMTP", self.smtp_port)];
        if self.mcp_permission != McpPermission::Disabled {
            ports.push(("MCP", self.mcp_port));
        }
        for (name, port) in &ports {
            if *port == 0 {
                return Err(format!("{name} port must be between 1 and 65535"));
            }
        }
        for (i, (a, pa)) in ports.iter().enumerate() {
            for (b, pb) in &ports[i + 1..] {
                if pa == pb {
                    return Err(format!("{a} and {b} ports must differ (both are {pa})"));
                }
            }
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            email: String::new(),
            imap_port: 1143,
            smtp_port: 1025,
            bind_addr: default_bind_addr(),
            api_url: default_api_url(),
            bridge_password: None,
            sync_limit: default_sync_limit(),
            mcp_permission: McpPermission::default(),
            mcp_port: default_mcp_port(),
            auto_update: default_auto_update(),
            session_encryption_method: SessionEncryptionMethod::default(),
            session_encryption_key: String::new(),
            session_unlock_file: None,
            session_unlock_port: default_unlock_port(),
            pass_path: None,
        }
    }
}

pub(crate) fn data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tutabridge")
}

/// Whether the Tuta session is kept in a file in the config directory instead
/// of the OS keychain. Meant for headless installs (containers) that have no
/// keychain: set `TUTABRIDGE_SESSION_STORE=file`.
pub fn session_store_is_file() -> bool {
    std::env::var("TUTABRIDGE_SESSION_STORE").is_ok_and(|v| v.trim().eq_ignore_ascii_case("file"))
}

pub fn default_unlock_file_path() -> PathBuf {
    data_dir().join("unlock")
}

pub fn session_file_path() -> PathBuf {
    data_dir().join("session.json")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn store_db_path() -> PathBuf {
    data_dir().join("store.db")
}

pub fn store_mails_dir() -> PathBuf {
    data_dir().join("mails")
}

pub fn load_config() -> Result<Option<Config>, Box<dyn std::error::Error>> {
    let path = config_path();
    if path.exists() {
        // The file holds the bridge password; heal installs whose config was
        // written before files were created owner-only.
        crate::fs_private::tighten(&path);
        let content = std::fs::read_to_string(&path)?;
        Ok(Some(toml::from_str(&content)?))
    } else {
        Ok(None)
    }
}

pub fn save_config(cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path();
    crate::fs_private::create_dir_all_private(path.parent().unwrap())?;
    let content = toml::to_string_pretty(cfg)?;
    crate::fs_private::write_private(&path, &content)?;
    Ok(())
}

/// Merge GUI-owned settings into the latest configuration on disk. Keep the
/// TOML table intact so fields this version does not know (such as pass_path
/// from a newer build) survive too.
pub fn save_settings(settings: &EditableSettings) -> Result<Config, Box<dyn std::error::Error>> {
    save_settings_at(&config_path(), settings)
}

fn save_settings_at(
    path: &std::path::Path,
    settings: &EditableSettings,
) -> Result<Config, Box<dyn std::error::Error>> {
    let existing = match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let (config, content) = merge_settings(existing.as_deref(), settings)?;
    crate::fs_private::create_dir_all_private(path.parent().unwrap())?;
    crate::fs_private::write_private(path, content)?;
    Ok(config)
}

/// Pure transformation: preserve hidden fields, apply the form and validate
/// before producing the content to persist. No filesystem or global state.
fn merge_settings(
    existing: Option<&str>,
    settings: &EditableSettings,
) -> Result<(Config, String), Box<dyn std::error::Error>> {
    let mut table: toml::Table = match existing {
        Some(content) => toml::from_str(content)?,
        None => toml::Table::new(),
    };
    table.extend(toml::Table::try_from(settings)?);
    let config: Config = table.clone().try_into()?;
    config.validate_ports()?;
    let content = toml::to_string_pretty(&table)?;
    Ok((config, content))
}

pub fn ensure_bridge_password(config: &mut Config) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(ref pw) = config.bridge_password {
        return Ok(pw.clone());
    }
    let password = generate_bridge_password();
    config.bridge_password = Some(password.clone());
    save_config(config)?;
    Ok(password)
}

pub fn regenerate_bridge_password(
    config: &mut Config,
) -> Result<String, Box<dyn std::error::Error>> {
    let password = generate_bridge_password();
    config.bridge_password = Some(password.clone());
    save_config(config)?;
    Ok(password)
}

fn generate_bridge_password() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    let mut group = || -> String {
        (0..5)
            .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
            .collect()
    };
    format!("{}-{}-{}-{}", group(), group(), group(), group())
}

#[cfg(test)]
fn parse_config(content: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_ports(imap: u16, smtp: u16, mcp: u16, mcp_permission: McpPermission) -> Config {
        Config {
            imap_port: imap,
            smtp_port: smtp,
            mcp_port: mcp,
            mcp_permission,
            ..Config::default()
        }
    }

    #[test]
    fn default_ports_are_valid() {
        assert_eq!(Config::default().validate_ports(), Ok(()));
    }

    #[test]
    fn imap_and_smtp_on_one_port_are_refused() {
        // The second bind would fail at start and take the bridge down.
        assert_eq!(
            with_ports(1143, 1143, 1944, McpPermission::Disabled).validate_ports(),
            Err("IMAP and SMTP ports must differ (both are 1143)".to_string())
        );
    }

    #[test]
    fn port_zero_is_refused() {
        // An emptied field became 0, and the system then picks a random port.
        assert_eq!(
            with_ports(1143, 0, 1944, McpPermission::Disabled).validate_ports(),
            Err("SMTP port must be between 1 and 65535".to_string())
        );
    }

    #[test]
    fn the_mcp_port_only_counts_when_mcp_is_enabled() {
        assert_eq!(
            with_ports(1143, 1025, 1025, McpPermission::Disabled).validate_ports(),
            Ok(())
        );
        assert_eq!(
            with_ports(1143, 1025, 1025, McpPermission::Metadata).validate_ports(),
            Err("SMTP and MCP ports must differ (both are 1025)".to_string())
        );
    }

    #[test]
    fn the_default_bind_address_is_loopback() {
        assert_eq!(Config::default().bind_addr, IpAddr::from([127, 0, 0, 1]));
    }

    #[test]
    fn the_bind_override_accepts_ips_and_ignores_blank_values() {
        assert_eq!(bind_override(None), Ok(None));
        assert_eq!(bind_override(Some("  ")), Ok(None));
        assert_eq!(
            bind_override(Some(" 0.0.0.0 ")),
            Ok(Some(IpAddr::from([0, 0, 0, 0])))
        );
        assert_eq!(
            bind_override(Some("::")),
            Ok(Some("::".parse::<IpAddr>().unwrap()))
        );
    }

    #[test]
    fn an_invalid_bind_override_is_reported() {
        assert!(bind_override(Some("not-an-ip")).is_err());
        assert!(bind_override(Some("0.0.0.0:1143")).is_err());
    }

    #[test]
    fn the_older_bind_address_key_is_still_read() {
        let base = "email = \"test@tuta.com\"\nimap_port = 1143\nsmtp_port = 1025\n";
        let cfg = parse_config(&format!("{base}bind_address = \"0.0.0.0\"\n")).unwrap();
        assert_eq!(cfg.bind_addr, IpAddr::from([0, 0, 0, 0]));
    }

    #[test]
    fn exposure_notices_follow_the_bind_address_and_password_form() {
        let plain = Config {
            bridge_password: Some("secret".into()),
            ..Config::default()
        };
        let hashed = Config {
            bridge_password: Some("$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA".into()),
            ..Config::default()
        };
        assert!(plain.notices_for(false, false).is_empty());
        assert_eq!(plain.notices_for(true, false).len(), 2);
        assert_eq!(hashed.notices_for(true, false).len(), 1);
        assert!(plain.notices_for(true, false)[1].1.contains("plaintext"));
    }

    #[test]
    fn an_unencrypted_session_file_is_noted_and_escalates_when_exposed() {
        let cfg = Config::default();
        assert_eq!(cfg.notices_for(false, false).len(), 0);
        let local = cfg.notices_for(false, true);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].0, log::Level::Info);
        assert_eq!(cfg.notices_for(true, true).last().unwrap().0, log::Level::Warn);
        let encrypted = Config {
            session_encryption_method: SessionEncryptionMethod::Web,
            ..Config::default()
        };
        assert!(encrypted.notices_for(false, true).is_empty());
    }

    #[test]
    fn test_default_config() {
        let cfg = Config::default();
        assert_eq!(cfg.email, "");
        assert_eq!(cfg.imap_port, 1143);
        assert_eq!(cfg.smtp_port, 1025);
        assert_eq!(cfg.bind_addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(cfg.imap_addr().to_string(), "127.0.0.1:1143");
        assert_eq!(cfg.smtp_addr().to_string(), "127.0.0.1:1025");
        assert_eq!(cfg.api_url, "https://app.tuta.com");
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
email = "test@tuta.com"
imap_port = 1993
smtp_port = 1587
api_url = "https://custom.tuta.com"
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg.email, "test@tuta.com");
        assert_eq!(cfg.imap_port, 1993);
        assert_eq!(cfg.smtp_port, 1587);
        assert_eq!(cfg.api_url, "https://custom.tuta.com");
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
email = "test@tuta.com"
imap_port = 1143
smtp_port = 1025
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg.email, "test@tuta.com");
        assert_eq!(cfg.api_url, "https://app.tuta.com");
        assert_eq!(cfg.bind_addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn mail_bind_address_accepts_ipv4_and_ipv6_literals() {
        let base = "email = \"test@tuta.com\"\nimap_port = 1143\nsmtp_port = 1025\n";
        for (ip, imap, smtp) in [
            ("0.0.0.0", "0.0.0.0:1143", "0.0.0.0:1025"),
            ("::1", "[::1]:1143", "[::1]:1025"),
            ("::", "[::]:1143", "[::]:1025"),
        ] {
            let cfg = parse_config(&format!("{base}bind_addr = \"{ip}\"\n")).unwrap();
            assert_eq!(cfg.imap_addr().to_string(), imap);
            assert_eq!(cfg.smtp_addr().to_string(), smtp);
            assert_eq!(cfg.validate_ports(), Ok(()));
        }
    }

    #[test]
    fn mail_bind_address_serialization_omits_only_the_default() {
        for ip in ["127.0.0.1", "0.0.0.0", "::1"] {
            let cfg = Config {
                bind_addr: ip.parse().unwrap(),
                ..Config::default()
            };
            let serialized = toml::to_string_pretty(&cfg).unwrap();
            let table: toml::Table = toml::from_str(&serialized).unwrap();
            assert_eq!(table.contains_key("bind_addr"), ip != "127.0.0.1");
            assert_eq!(parse_config(&serialized).unwrap(), cfg);
        }
    }

    #[test]
    fn mail_bind_address_rejects_hostnames_ports_and_invalid_values() {
        let base = "email = \"test@tuta.com\"\nimap_port = 1143\nsmtp_port = 1025\n";
        for value in ["localhost", "127.0.0.1:1143", "[::1]", "256.0.0.1", ""] {
            assert!(
                parse_config(&format!("{base}bind_addr = \"{value}\"\n")).is_err(),
                "{value}"
            );
        }
        assert!(parse_config(&format!("{base}bind_addr = 123\n")).is_err());
    }

    #[test]
    fn test_parse_config_missing_email() {
        let toml = r#"
imap_port = 1143
smtp_port = 1025
"#;
        let result = parse_config(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_config_invalid_port_type() {
        let toml = r#"
email = "test@tuta.com"
imap_port = "not_a_number"
smtp_port = 1025
"#;
        let result = parse_config(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_roundtrip() {
        let cfg = Config {
            email: "roundtrip@tuta.com".to_string(),
            imap_port: 2143,
            smtp_port: 2025,
            bind_addr: "::1".parse().unwrap(),
            api_url: "https://app.tuta.com".to_string(),
            bridge_password: None,
            sync_limit: 500,
            mcp_permission: McpPermission::Full,
            mcp_port: 1944,
            auto_update: false,
            session_encryption_method: SessionEncryptionMethod::None,
            session_encryption_key: String::new(),
            session_unlock_file: None,
            session_unlock_port: 8080,
            pass_path: Some("Tuta/Bridge".to_string()),
        };
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(cfg, deserialized);
    }

    #[test]
    fn test_mcp_permission_defaults_disabled() {
        let toml = r#"
email = "test@tuta.com"
imap_port = 1143
smtp_port = 1025
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg.mcp_permission, McpPermission::Disabled);
        assert!(!cfg.mcp_permission.is_enabled());
        assert_eq!(cfg.mcp_port, 1944);
    }

    #[test]
    fn test_mcp_permission_parses_and_gates_body() {
        let toml = r#"
email = "test@tuta.com"
imap_port = 1143
smtp_port = 1025
mcp_permission = "metadata"
mcp_port = 9999
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg.mcp_permission, McpPermission::Metadata);
        assert!(cfg.mcp_permission.is_enabled());
        assert!(!cfg.mcp_permission.allows_body());
        assert_eq!(cfg.mcp_port, 9999);
    }

    #[test]
    fn auto_update_is_on_unless_the_config_turns_it_off() {
        let base = "email = \"test@tuta.com\"\nimap_port = 1143\nsmtp_port = 1025\n";
        assert!(parse_config(base).unwrap().auto_update);
        let off = format!("{base}auto_update = false\n");
        assert!(!parse_config(&off).unwrap().auto_update);
    }

    #[test]
    fn test_parse_config_extra_fields_ignored() {
        let toml = r#"
email = "test@tuta.com"
imap_port = 1143
smtp_port = 1025
unknown_field = "ignored"
"#;
        // toml by default errors on unknown fields with deny_unknown_fields,
        // but serde default is to ignore them
        let result = parse_config(toml);
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    // This is the payload emitted by ConfigPanel, without hidden fields.
    const FORM: &str = r#"{
        "email":"me@tuta.io", "imap_port":2143, "smtp_port":2025,
        "api_url":"https://app.tuta.com", "sync_limit":0,
        "mcp_permission":"metadata", "mcp_port":2944, "auto_update":false
    }"#;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "tutabridge-settings-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            Self(dir.join("config.toml"))
        }

        fn write(&self, content: &str) {
            crate::fs_private::create_dir_all_private(self.0.parent().unwrap()).unwrap();
            crate::fs_private::write_private(&self.0, content).unwrap();
        }

        fn read(&self) -> String {
            std::fs::read_to_string(&self.0).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    const EXISTING: &str = r#"email = "old@tuta.io"
imap_port = 1143
smtp_port = 1025
api_url = "https://old.example.invalid"
sync_limit = 500
mcp_permission = "disabled"
mcp_port = 1944
auto_update = true
bridge_password = "keep-me"
pass_path = "Tuta/Bridge"
bind_addr = "0.0.0.0"
[future_settings]
enabled = true
"#;

    #[test]
    fn merge_preserves_hidden_fields_and_applies_every_form_setting() {
        let settings: EditableSettings = serde_json::from_str(FORM).unwrap();
        let (config, content) = merge_settings(Some(EXISTING), &settings).unwrap();
        assert_eq!(config.email, "me@tuta.io");
        assert_eq!(config.imap_port, 2143);
        assert_eq!(config.smtp_port, 2025);
        assert_eq!(config.api_url, "https://app.tuta.com");
        assert_eq!(config.sync_limit, 0);
        assert_eq!(config.mcp_permission, McpPermission::Metadata);
        assert_eq!(config.mcp_port, 2944);
        assert!(!config.auto_update);
        assert_eq!(config.bridge_password.as_deref(), Some("keep-me"));
        assert_eq!(config.bind_addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let merged: toml::Table = toml::from_str(&content).unwrap();
        assert_eq!(merged["bridge_password"].as_str(), Some("keep-me"));
        assert_eq!(merged["pass_path"].as_str(), Some("Tuta/Bridge"));
        assert_eq!(merged["bind_addr"].as_str(), Some("0.0.0.0"));
        assert_eq!(merged["future_settings"]["enabled"].as_bool(), Some(true));
        assert_eq!(config, toml::from_str::<Config>(&content).unwrap());
    }

    #[test]
    fn merge_without_existing_content_creates_a_first_run_config() {
        let settings: EditableSettings = serde_json::from_str(FORM).unwrap();
        let (config, content) = merge_settings(None, &settings).unwrap();
        assert_eq!(config.email, "me@tuta.io");
        assert_eq!(config.imap_port, 2143);
        assert_eq!(config.bridge_password, None);
        assert_eq!(config.bind_addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(config, toml::from_str::<Config>(&content).unwrap());
    }

    #[test]
    fn merge_rejects_invalid_ports() {
        for (imap, smtp, mcp) in [(0, 2025, 2944), (2143, 2143, 2944), (2143, 2025, 2143)] {
            let mut settings: EditableSettings = serde_json::from_str(FORM).unwrap();
            settings.imap_port = imap;
            settings.smtp_port = smtp;
            settings.mcp_port = mcp;
            assert!(merge_settings(Some(EXISTING), &settings).is_err());
        }
    }

    #[test]
    fn merge_rejects_corrupt_or_ill_typed_existing_content() {
        let settings: EditableSettings = serde_json::from_str(FORM).unwrap();
        for content in ["invalid [toml", "bridge_password = 123"] {
            assert!(merge_settings(Some(content), &settings).is_err());
        }
    }

    #[test]
    fn save_creates_a_private_file_and_reads_the_latest_hidden_values() {
        let file = Fixture::new();
        let settings: EditableSettings = serde_json::from_str(FORM).unwrap();
        let first = save_settings_at(&file.0, &settings).unwrap();
        assert_eq!(first, toml::from_str::<Config>(&file.read()).unwrap());
        // A password regenerated after opening the form must be retained.
        file.write(&EXISTING.replace("keep-me", "regenerated-password"));
        let saved = save_settings_at(&file.0, &settings).unwrap();
        assert_eq!(
            saved.bridge_password.as_deref(),
            Some("regenerated-password")
        );
        assert_eq!(saved.imap_port, 2143);
        let persisted: toml::Table = toml::from_str(&file.read()).unwrap();
        assert_eq!(persisted["pass_path"].as_str(), Some("Tuta/Bridge"));
        assert_eq!(persisted["bind_addr"].as_str(), Some("0.0.0.0"));
        assert_eq!(saved, toml::from_str::<Config>(&file.read()).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file.0).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_save_leaves_the_existing_file_untouched() {
        let file = Fixture::new();
        let mut settings: EditableSettings = serde_json::from_str(FORM).unwrap();
        file.write(EXISTING);
        settings.imap_port = settings.smtp_port;
        assert!(save_settings_at(&file.0, &settings).is_err());
        assert_eq!(file.read(), EXISTING);

        settings.imap_port = 2143;
        file.write("invalid [toml");
        assert!(save_settings_at(&file.0, &settings).is_err());
        assert_eq!(file.read(), "invalid [toml");
        // An unreadable path must not be mistaken for a missing file.
        std::fs::remove_file(&file.0).unwrap();
        std::fs::create_dir(&file.0).unwrap();
        assert!(save_settings_at(&file.0, &settings).is_err());
        assert!(file.0.is_dir());
    }

    #[test]
    fn form_payload_cannot_overwrite_hidden_fields_or_omit_settings() {
        let mut payload: serde_json::Value = serde_json::from_str(FORM).unwrap();
        payload["bridge_password"] = serde_json::Value::String("stale-password".into());
        assert!(serde_json::from_value::<EditableSettings>(payload.clone()).is_err());
        payload.as_object_mut().unwrap().remove("bridge_password");
        payload["bind_addr"] = serde_json::Value::String("0.0.0.0".into());
        assert!(serde_json::from_value::<EditableSettings>(payload.clone()).is_err());
        payload.as_object_mut().unwrap().remove("bind_addr");
        payload.as_object_mut().unwrap().remove("auto_update");
        assert!(serde_json::from_value::<EditableSettings>(payload).is_err());
    }
}
