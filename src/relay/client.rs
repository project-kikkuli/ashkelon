use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

/// Request bodies are always fully buffered before ashkelon forwards them, so `Full` is enough —
/// only responses need to stream.
pub type UpstreamClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// Builds the client ashkelon uses to reach real providers: native root certs, falling back to the
/// bundled webpki set when the platform store can't be loaded, http1/http2 negotiated via ALPN.
pub fn build() -> anyhow::Result<UpstreamClient> {
    let with_schemes = HttpsConnectorBuilder::new()
        .with_native_roots()
        .unwrap_or_else(|_| HttpsConnectorBuilder::new().with_webpki_roots());
    let https = with_schemes.https_or_http().enable_http1().enable_http2().build();
    Ok(Client::builder(TokioExecutor::new()).build(https))
}
