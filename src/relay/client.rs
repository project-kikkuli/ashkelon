use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

/// Request bodies are always fully buffered before ashkelon forwards them, so `Full` is enough —
/// only responses need to stream.
pub type UpstreamClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// Builds the client ashkelon uses to reach real providers: the bundled webpki roots plus whatever
/// the platform store yields, http1/http2 negotiated via ALPN.
pub fn build() -> anyhow::Result<UpstreamClient> {
    let roots = root_store(rustls_native_certs::load_native_certs().certs);
    let tls = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(https))
}

/// The platform store can come back partial (e.g. a keychain not yet readable at boot), so it only
/// ever adds to the webpki set.
fn root_store(native: Vec<CertificateDer<'static>>) -> RootCertStore {
    let mut roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    roots.add_parsable_certificates(native);
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webpki_roots_survive_a_partial_native_store() {
        for native in [vec![], vec![CertificateDer::from(vec![0u8; 8])]] {
            assert_eq!(root_store(native).len(), webpki_roots::TLS_SERVER_ROOTS.len());
        }
    }
}
