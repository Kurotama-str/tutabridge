use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

fn cert_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tutabridge")
}

fn cert_path() -> PathBuf {
    cert_dir().join("cert.pem")
}

fn key_path() -> PathBuf {
    cert_dir().join("key.pem")
}

pub fn load_or_create_tls_acceptor() -> Result<TlsAcceptor, Box<dyn std::error::Error + Send + Sync>>
{
    let cert_file = cert_path();
    let key_file = key_path();

    let (cert_pem, key_pem) = if cert_file.exists() && key_file.exists() {
        log::info!("Loading TLS certificate from {}", cert_file.display());
        // Heal installs whose key was written before it was created owner-only.
        crate::fs_private::tighten(&key_file);
        let cert_pem = std::fs::read_to_string(&cert_file)?;
        let key_pem = std::fs::read_to_string(&key_file)?;
        warn_if_blank(&cert_file, &cert_pem);
        warn_if_blank(&key_file, &key_pem);
        (cert_pem, key_pem)
    } else {
        log::info!("Generating self-signed TLS certificate...");
        let (cert, key) = generate_self_signed()?;
        crate::fs_private::create_dir_all_private(cert_dir())?;
        std::fs::write(&cert_file, &cert)?;
        crate::fs_private::write_private(&key_file, &key)?;
        log::info!("Certificate saved to {}", cert_file.display());
        (cert, key)
    };

    let certs = load_certs(&cert_pem)?;
    let key = load_key(&key_pem)?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Warn about a certificate or key file that exists but holds nothing, as seen
/// by the bridge itself. A file replaced by a bind mount is read with its mounted
/// content, so an empty placeholder underneath a working mount never triggers
/// this. The load below still fails; the warning says which file and why.
fn warn_if_blank(path: &std::path::Path, content: &str) -> bool {
    let blank = content.trim().is_empty();
    if blank {
        log::warn!(
            "{} is empty, so TLS setup will fail. Fill it with the real PEM, or delete \
             both cert.pem and key.pem to get a self-signed certificate. If it should \
             be a bind mount, check that the mount is in place.",
            path.display()
        );
    }
    blank
}

fn generate_self_signed() -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])?;
    params.subject_alt_names = vec![
        rcgen::SanType::DnsName("localhost".try_into()?),
        rcgen::SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
    ];
    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

fn load_certs(
    pem: &str,
) -> Result<Vec<CertificateDer<'static>>, Box<dyn std::error::Error + Send + Sync>> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err("No certificates found in PEM".into());
    }
    Ok(certs)
}

/// The first private key in `pem`, in any of the usual encodings (PKCS#8,
/// PKCS#1 RSA, SEC1 EC), so a certificate authority's key file works as is.
fn load_key(pem: &str) -> Result<PrivateKeyDer<'static>, Box<dyn std::error::Error + Send + Sync>> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)?.ok_or_else(|| "No private key found in PEM".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_blank_files_are_reported_empty() {
        let path = std::path::Path::new("cert.pem");
        assert!(warn_if_blank(path, ""));
        assert!(warn_if_blank(path, " \n\t\n"));
        assert!(!warn_if_blank(path, "-----BEGIN CERTIFICATE-----"));
    }

    #[test]
    fn test_tls_acceptor_builds() {
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
        let (cert_pem, key_pem) = generate_self_signed().unwrap();
        let certs = load_certs(&cert_pem).unwrap();
        let key = load_key(&key_pem).unwrap();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key);
        assert!(config.is_ok());
    }
}
