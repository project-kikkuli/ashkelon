use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::{Connected, HttpConnector};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use tower::Service;

/// Bounds TCP connect plus TLS handshake. Successful connects take milliseconds; one still pending
/// after this is a stall.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Pause before the single connect retry, so a connector that just dropped has a moment to recover.
pub const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Request bodies are always fully buffered before ashkelon forwards them, so `Full` is enough —
/// only responses need to stream.
pub type BoundedConnector = Bounded<HttpsConnector<HttpConnector>>;

pub type UpstreamClient = Client<BoundedConnector, Full<Bytes>>;

/// Builds the client ashkelon uses to reach real providers: the bundled webpki roots plus whatever
/// the platform store yields, http1/http2 negotiated via ALPN.
pub fn build() -> anyhow::Result<UpstreamClient> {
    let roots = root_store(rustls_native_certs::load_native_certs().certs);
    let tls = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http);
    Ok(Client::builder(TokioExecutor::new()).build(bounded(https)))
}

/// Wraps `connector` so a connect that does not finish within [`CONNECT_TIMEOUT`] fails as a
/// connect error.
pub fn bounded(connector: HttpsConnector<HttpConnector>) -> BoundedConnector {
    Bounded(connector)
}

/// A connector whose whole connect (TCP plus TLS) is bounded by [`CONNECT_TIMEOUT`].
#[derive(Clone)]
pub struct Bounded<C>(C);

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl<C> Service<http::Uri> for Bounded<C>
where
    C: Service<http::Uri>,
    C::Future: Send + 'static,
    C::Error: Into<BoxError>,
    C::Response: 'static,
{
    type Response = C::Response;
    type Error = BoxError;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<C::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), BoxError>> {
        self.0.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let connecting = self.0.call(uri);
        Box::pin(async move {
            match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
                Ok(connected) => connected.map_err(Into::into),
                Err(_) => Err(format!("connect timed out after {}s", CONNECT_TIMEOUT.as_secs()).into()),
            }
        })
    }
}

pub(super) fn retire_on_protocol_error(
    error: &(dyn std::error::Error + 'static),
    connection: Option<&Connected>,
) -> bool {
    let remote_protocol_error = std::iter::successors(Some(error), |cause| cause.source())
        .filter_map(|cause| cause.downcast_ref::<h2::Error>())
        .any(|error| error.is_remote() && error.reason() == Some(h2::Reason::PROTOCOL_ERROR));
    if remote_protocol_error {
        if let Some(connection) = connection {
            connection.poison();
            return true;
        }
    }
    false
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
