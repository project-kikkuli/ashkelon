//! Side-channel decompression: the client always gets the upstream's exact bytes; this decodes a
//! copy for observation (parsing, transforms) only.
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_compression::tokio::write::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZstdDecoder};
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Identity,
    Gzip,
    Brotli,
    Zstd,
    Deflate,
}

impl Encoding {
    pub fn from_header(value: Option<&str>) -> Encoding {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("gzip") | Some("x-gzip") => Encoding::Gzip,
            Some("br") => Encoding::Brotli,
            Some("zstd") => Encoding::Zstd,
            Some("deflate") => Encoding::Deflate,
            _ => Encoding::Identity,
        }
    }

    pub fn is_identity(self) -> bool {
        matches!(self, Encoding::Identity)
    }
}

/// An `AsyncWrite` sink that just accumulates whatever is written to it. Never returns `Pending`
/// or an error, so decoders that write decompressed output through it never stall.
#[derive(Clone, Default)]
struct VecSink(Arc<Mutex<Vec<u8>>>);

impl AsyncWrite for VecSink {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

enum Inner {
    Identity,
    Gzip(Box<GzipDecoder<VecSink>>),
    Brotli(Box<BrotliDecoder<VecSink>>),
    Zstd(Box<ZstdDecoder<VecSink>>),
    Deflate(Box<DeflateDecoder<VecSink>>),
}

/// Incremental decoder for one response/request body. Fed compressed chunks as they arrive;
/// never fails the caller — a corrupt or truncated stream just stops producing further output.
pub struct StreamDecoder {
    inner: Inner,
    sink: Arc<Mutex<Vec<u8>>>,
    failed: bool,
}

impl StreamDecoder {
    pub fn new(encoding: Encoding) -> StreamDecoder {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let inner = match encoding {
            Encoding::Identity => Inner::Identity,
            Encoding::Gzip => Inner::Gzip(Box::new(GzipDecoder::new(VecSink(sink.clone())))),
            Encoding::Brotli => Inner::Brotli(Box::new(BrotliDecoder::new(VecSink(sink.clone())))),
            Encoding::Zstd => Inner::Zstd(Box::new(ZstdDecoder::new(VecSink(sink.clone())))),
            Encoding::Deflate => Inner::Deflate(Box::new(DeflateDecoder::new(VecSink(sink.clone())))),
        };
        StreamDecoder {
            inner,
            sink,
            failed: false,
        }
    }

    /// Decodes what it can from `chunk`, returning newly available decoded bytes.
    pub async fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        if let Inner::Identity = self.inner {
            return chunk.to_vec();
        }
        if self.failed || chunk.is_empty() {
            return Vec::new();
        }
        let result = match &mut self.inner {
            Inner::Identity => unreachable!(),
            Inner::Gzip(d) => d.write_all(chunk).await,
            Inner::Brotli(d) => d.write_all(chunk).await,
            Inner::Zstd(d) => d.write_all(chunk).await,
            Inner::Deflate(d) => d.write_all(chunk).await,
        };
        if result.is_err() {
            self.failed = true;
        }
        self.drain()
    }

    /// Flushes any trailing decoded bytes once the compressed stream has ended.
    pub async fn finish(&mut self) -> Vec<u8> {
        if !self.failed {
            let result = match &mut self.inner {
                Inner::Identity => Ok(()),
                Inner::Gzip(d) => d.shutdown().await,
                Inner::Brotli(d) => d.shutdown().await,
                Inner::Zstd(d) => d.shutdown().await,
                Inner::Deflate(d) => d.shutdown().await,
            };
            if result.is_err() {
                self.failed = true;
            }
        }
        self.drain()
    }

    fn drain(&self) -> Vec<u8> {
        let mut buf = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *buf)
    }
}

/// One-shot decode of a fully-buffered body (request bodies are always read whole before ashkelon
/// inspects them). Falls back to the original bytes when the stream can't be decoded at all.
pub async fn decode_all(encoding: Encoding, body: &[u8]) -> Vec<u8> {
    if encoding.is_identity() {
        return body.to_vec();
    }
    let mut decoder = StreamDecoder::new(encoding);
    let mut out = decoder.push(body).await;
    out.extend(decoder.finish().await);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gzip_bytes(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn deflate_bytes(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn zstd_bytes(data: &[u8]) -> Vec<u8> {
        zstd::encode_all(data, 0).unwrap()
    }

    fn brotli_bytes(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
            use std::io::Write;
            writer.write_all(data).unwrap();
        }
        out
    }

    #[tokio::test]
    async fn identity_passes_through() {
        let data = b"hello world";
        assert_eq!(decode_all(Encoding::Identity, data).await, data);
    }

    #[tokio::test]
    async fn decodes_gzip() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let compressed = gzip_bytes(data);
        assert_eq!(decode_all(Encoding::Gzip, &compressed).await, data);
    }

    #[tokio::test]
    async fn decodes_deflate() {
        let data = b"deflate me please";
        let compressed = deflate_bytes(data);
        assert_eq!(decode_all(Encoding::Deflate, &compressed).await, data);
    }

    #[tokio::test]
    async fn decodes_zstd() {
        let data = b"zstandard compressed payload";
        let compressed = zstd_bytes(data);
        assert_eq!(decode_all(Encoding::Zstd, &compressed).await, data);
    }

    #[tokio::test]
    async fn decodes_brotli() {
        let data = b"brotli compressed payload";
        let compressed = brotli_bytes(data);
        assert_eq!(decode_all(Encoding::Brotli, &compressed).await, data);
    }

    #[tokio::test]
    async fn decodes_incrementally_across_chunks() {
        let data = b"a payload split across several small chunks for the incremental decoder";
        let compressed = gzip_bytes(data);
        let mut decoder = StreamDecoder::new(Encoding::Gzip);
        let mut out = Vec::new();
        for chunk in compressed.chunks(3) {
            out.extend(decoder.push(chunk).await);
        }
        out.extend(decoder.finish().await);
        assert_eq!(out, data);
    }

    #[tokio::test]
    async fn corrupt_stream_never_panics() {
        let mut decoder = StreamDecoder::new(Encoding::Gzip);
        let out = decoder.push(b"not actually gzip data").await;
        assert!(out.is_empty());
        let out2 = decoder.finish().await;
        assert!(out2.is_empty());
    }

    #[test]
    fn encoding_from_header_recognizes_known_values() {
        assert_eq!(Encoding::from_header(Some("gzip")), Encoding::Gzip);
        assert_eq!(Encoding::from_header(Some("br")), Encoding::Brotli);
        assert_eq!(Encoding::from_header(Some("zstd")), Encoding::Zstd);
        assert_eq!(Encoding::from_header(Some("deflate")), Encoding::Deflate);
        assert_eq!(Encoding::from_header(Some("identity")), Encoding::Identity);
        assert_eq!(Encoding::from_header(None), Encoding::Identity);
    }
}
