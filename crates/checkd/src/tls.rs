//! TLS for remote workers: the engine's server side, the worker's client
//! side, and `checkd make-certs`, which makes a certificate authority and a
//! certificate for the engine, for a site that has none of its own.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::{TlsAcceptor, TlsConnector};

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    CertificateDer::pem_file_iter(path)
        .map_err(|err| format!("cannot read the certificates in {}: {err}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("{}: {err}", path.display()))
}

/// The engine's side: its certificate and key.
pub fn acceptor(cert: &Path, key: &Path) -> Result<TlsAcceptor, String> {
    let key = PrivateKeyDer::from_pem_file(key).map_err(|err| format!("cannot read the key {}: {err}", key.display()))?;
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs(cert)?, key)
        .map_err(|err| err.to_string())?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// The worker's side: the authority whose certificates it trusts.
pub fn connector(ca: &Path) -> Result<TlsConnector, String> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in certs(ca)? {
        roots.add(cert).map_err(|err| format!("{}: {err}", ca.display()))?;
    }
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

pub fn server_name(host: &str) -> Result<ServerName<'static>, String> {
    ServerName::try_from(host.to_string()).map_err(|err| format!("'{host}' is not a host name: {err}"))
}

/// `checkd make-certs <dir> <name>...`: writes `ca.crt` and `ca.key`, and
/// `engine.crt` and `engine.key` for the host names given, into the
/// directory. The CA's key is to be kept away from the engine; workers need
/// `ca.crt` only.
pub fn make_certs(dir: &Path, names: &[String]) -> Result<(), String> {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
    std::fs::create_dir_all(dir).map_err(|err| format!("cannot make {}: {err}", dir.display()))?;
    let write = |name: &str, text: String, secret: bool| -> Result<(), String> {
        use std::io::Write;
        let path = dir.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // A key is never readable by others, not even for a moment.
        #[cfg(unix)]
        if secret {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = secret;
        options
            .open(&path)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .map_err(|err| format!("cannot write {}: {err}", path.display()))
    };
    let ca_key = KeyPair::generate().map_err(|err| err.to_string())?;
    let mut ca = CertificateParams::new(Vec::<String>::new()).map_err(|err| err.to_string())?;
    ca.distinguished_name.push(DnType::CommonName, "checkd workers CA");
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca.self_signed(&ca_key).map_err(|err| err.to_string())?;
    let issuer = rcgen::Issuer::new(ca, &ca_key);

    let engine_key = KeyPair::generate().map_err(|err| err.to_string())?;
    let mut engine = CertificateParams::new(names.to_vec()).map_err(|err| err.to_string())?;
    engine.distinguished_name.push(DnType::CommonName, names.first().cloned().unwrap_or_default());
    let engine_cert = engine.signed_by(&engine_key, &issuer).map_err(|err| err.to_string())?;

    write("ca.crt", ca_cert.pem(), false)?;
    write("ca.key", ca_key.serialize_pem(), true)?;
    write("engine.crt", engine_cert.pem(), false)?;
    write("engine.key", engine_key.serialize_pem(), true)?;
    Ok(())
}
