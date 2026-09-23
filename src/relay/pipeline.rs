use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http::header::HOST;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::config::Config;
use crate::hooks::Engine;
use crate::rules::{self, PreDecision, StreamGuard};
use crate::session::{self, SessionKey};
use crate::telemetry::{now_ts, BodyKind, CallRecord, Writer};
use crate::transform;
use crate::usage::{self, Usage};
use crate::wire::Wire;

use super::client::UpstreamClient;
use super::decode::{decode_all, Encoding, StreamDecoder};
use super::route;

pub type ResponseBody = BoxBody<Bytes, Infallible>;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

pub async fn handle(
    req: Request<Incoming>,
    cfg: Arc<Config>,
    engine: Arc<Engine>,
    client: UpstreamClient,
    telemetry: Arc<Writer>,
) -> Result<Response<ResponseBody>, Infallible> {
    let call_id = uuid::Uuid::new_v4().to_string();
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let in_headers = req.headers().clone();

    let Some(parsed) = route::resolve(&path, &cfg) else {
        return Ok(not_found());
    };
    let wire = Wire::from_path(&parsed.rest);

    let raw_request = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => return Ok(bad_gateway(&format!("reading request body: {err}"))),
    };
    let request_bytes = raw_request.len() as u64;

    let request_encoding = Encoding::from_header(header_str(&in_headers, "content-encoding"));
    let decoded_request = decode_all(request_encoding, &raw_request).await;

    let key = session::derive(parsed.launch.as_deref(), wire, &in_headers, &decoded_request);
    engine.observe_request(&key, wire, &decoded_request);

    let applied = transform::apply(wire, &cfg.transforms, &decoded_request);
    let transform_names = applied.changed;
    let mut current_body = applied.body;

    if let PreDecision::Reject { rule, message } = rules::check_request(wire, &cfg.rules, &current_body) {
        let (status, resp_body) = rules::reject_response(wire, &rule, &message);
        let record = CallRecord {
            ts: now_ts(),
            call_id: call_id.clone(),
            session: key,
            route: parsed.route,
            wire,
            method: method.to_string(),
            path,
            status,
            model: None,
            stop_reason: None,
            usage: Usage::default(),
            tool_calls: Vec::new(),
            turn_end: false,
            ttfb_ms: None,
            total_ms: 0,
            request_bytes,
            response_bytes: resp_body.len() as u64,
            transforms: transform_names,
            pings_injected: Vec::new(),
            rule: Some(rule),
            error: None,
        };
        log_record(&telemetry, &record);
        if cfg.log_bodies {
            log_body(&telemetry, &call_id, BodyKind::Request, &raw_request);
        }
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
        return Ok(json_response(status, resp_body));
    }

    let mut pings_injected = Vec::new();
    if let Some((body, ids)) = engine.attach_pings(&key, wire, &current_body) {
        current_body = body;
        pings_injected = ids;
    }

    let modified = !transform_names.is_empty() || !pings_injected.is_empty();
    let (outgoing_body, mut out_headers) = if modified {
        let mut headers = strip_hop_by_hop(&in_headers);
        headers.remove("content-encoding");
        headers.remove("content-length");
        headers.insert(
            http::header::CONTENT_LENGTH,
            HeaderValue::from_str(&current_body.len().to_string()).unwrap(),
        );
        (current_body.clone(), headers)
    } else {
        (raw_request.to_vec(), strip_hop_by_hop(&in_headers))
    };
    out_headers.remove(HOST);

    let Some(upstream_uri) = build_upstream_uri(&parsed.upstream, &parsed.rest, &query) else {
        return Ok(bad_gateway("invalid upstream route"));
    };
    if let Some(host_value) = host_header_value(&upstream_uri) {
        out_headers.insert(HOST, host_value);
    }

    let mut builder = Request::builder().method(method.clone()).uri(upstream_uri);
    *builder.headers_mut().unwrap() = out_headers;
    let upstream_req = match builder.body(Full::new(Bytes::from(outgoing_body))) {
        Ok(r) => r,
        Err(err) => return Ok(bad_gateway(&format!("building upstream request: {err}"))),
    };

    let t0 = Instant::now();
    let upstream_resp = match client.request(upstream_req).await {
        Ok(r) => r,
        Err(err) => {
            let record = CallRecord {
                ts: now_ts(),
                call_id: call_id.clone(),
                session: key,
                route: parsed.route,
                wire,
                method: method.to_string(),
                path,
                status: 502,
                model: None,
                stop_reason: None,
                usage: Usage::default(),
                tool_calls: Vec::new(),
                turn_end: false,
                ttfb_ms: None,
                total_ms: t0.elapsed().as_millis() as u64,
                request_bytes,
                response_bytes: 0,
                transforms: transform_names,
                pings_injected,
                rule: None,
                error: Some(format!("upstream connect failed: {err}")),
            };
            log_record(&telemetry, &record);
            return Ok(bad_gateway(&format!("connecting upstream: {err}")));
        }
    };

    let (resp_parts, incoming_body) = upstream_resp.into_parts();
    let status = resp_parts.status;
    let response_encoding = Encoding::from_header(header_str(&resp_parts.headers, "content-encoding"));
    let mut client_headers = strip_hop_by_hop(&resp_parts.headers);
    // A stream a rule may cut can end early, so it can't promise a length.
    let cuttable = cfg.rules.max_response_chars.is_some() || !cfg.rules.cut_patterns.is_empty();
    if cuttable && wire != Wire::Opaque {
        client_headers.remove(http::header::CONTENT_LENGTH);
    }

    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(8);

    let log_bodies = cfg.log_bodies;
    let request_log_bytes = if log_bodies { Some(raw_request.to_vec()) } else { None };
    let engine_bg = engine.clone();
    let telemetry_bg = telemetry.clone();
    let cfg_rules = cfg.clone();

    tokio::spawn(async move {
        forward_response(ForwardArgs {
            call_id,
            key,
            route: parsed.route,
            wire,
            method: method.to_string(),
            path,
            status: status.as_u16(),
            request_bytes,
            transforms: transform_names,
            pings_injected,
            t0,
            incoming_body,
            response_encoding,
            tx,
            engine: engine_bg,
            telemetry: telemetry_bg,
            cfg: cfg_rules,
            log_bodies,
            request_log_bytes,
        })
        .await;
    });

    let stream_body = StreamBody::new(ReceiverStream::new(rx));
    let mut response = Response::builder().status(status).body(BoxBody::new(stream_body)).unwrap();
    *response.headers_mut() = client_headers;
    Ok(response)
}

struct ForwardArgs {
    call_id: String,
    key: SessionKey,
    route: String,
    wire: Wire,
    method: String,
    path: String,
    status: u16,
    request_bytes: u64,
    transforms: Vec<String>,
    pings_injected: Vec<String>,
    t0: Instant,
    incoming_body: Incoming,
    response_encoding: Encoding,
    tx: mpsc::Sender<Result<Frame<Bytes>, Infallible>>,
    engine: Arc<Engine>,
    telemetry: Arc<Writer>,
    cfg: Arc<Config>,
    log_bodies: bool,
    request_log_bytes: Option<Vec<u8>>,
}

async fn forward_response(args: ForwardArgs) {
    let mut incoming_body = args.incoming_body;
    let mut parser = usage::parser_for(args.wire);
    let mut guard = StreamGuard::new(&args.cfg.rules);
    let mut decoder = parser.as_ref().map(|_| StreamDecoder::new(args.response_encoding));

    let mut first_byte_at: Option<Instant> = None;
    let mut response_bytes: u64 = 0;
    let mut cut_rule: Option<String> = None;
    let mut error: Option<String> = None;
    let mut response_log_bytes = if args.log_bodies { Some(Vec::new()) } else { None };

    loop {
        let frame = match http_body_util::BodyExt::frame(&mut incoming_body).await {
            Some(Ok(frame)) => frame,
            Some(Err(err)) => {
                error = Some(format!("upstream read error: {err}"));
                break;
            }
            None => break,
        };

        let Some(data) = frame.data_ref() else {
            continue;
        };
        if first_byte_at.is_none() {
            first_byte_at = Some(Instant::now());
        }
        response_bytes += data.len() as u64;
        if let Some(buf) = response_log_bytes.as_mut() {
            buf.extend_from_slice(data);
        }

        if let (Some(parser_ref), Some(dec)) = (parser.as_deref_mut(), decoder.as_mut()) {
            let decoded = dec.push(data).await;
            parser_ref.feed(&decoded);
            if let Some(rule) = guard.check(parser_ref) {
                cut_rule = Some(rule);
            }
        }

        // The chunk that tripped a rule is withheld, so the agent never sees the offending output.
        if let Some(rule) = &cut_rule {
            // A plaintext tail can't follow compressed bytes; a compressed stream just ends.
            let tail = if args.response_encoding.is_identity() { rules::cut_tail(args.wire, rule) } else { Vec::new() };
            if !tail.is_empty() {
                let _ = args.tx.send(Ok(Frame::data(Bytes::from(tail)))).await;
            }
            break;
        }

        if args.tx.send(Ok(Frame::data(data.clone()))).await.is_err() {
            error.get_or_insert_with(|| "client disconnected".to_string());
            break;
        }
    }
    drop(args.tx);
    drop(incoming_body);

    if let Some(dec) = decoder.as_mut() {
        if let Some(parser_ref) = parser.as_deref_mut() {
            let tail = dec.finish().await;
            parser_ref.feed(&tail);
        }
    }

    let summary = parser.map(|p| p.finish());
    let total_ms = args.t0.elapsed().as_millis() as u64;
    let ttfb_ms = first_byte_at.map(|t| t.duration_since(args.t0).as_millis() as u64);

    if let Some(s) = summary.as_ref() {
        args.engine.observe_response(&args.key, args.wire, s);
    }

    let (model, stop_reason, usage, tool_calls, turn_end, parser_error) = match summary {
        Some(s) => (
            s.model,
            s.stop_reason,
            s.usage,
            s.tool_calls.into_iter().map(|t| t.name).collect(),
            s.turn_end,
            s.error,
        ),
        None => (None, None, Usage::default(), Vec::new(), false, None),
    };
    let error = error.or(parser_error);

    let record = CallRecord {
        ts: now_ts(),
        call_id: args.call_id.clone(),
        session: args.key,
        route: args.route,
        wire: args.wire,
        method: args.method,
        path: args.path,
        status: args.status,
        model,
        stop_reason,
        usage,
        tool_calls,
        turn_end,
        ttfb_ms,
        total_ms,
        request_bytes: args.request_bytes,
        response_bytes,
        transforms: args.transforms,
        pings_injected: args.pings_injected,
        rule: cut_rule,
        error,
    };

    log_record(&args.telemetry, &record);
    if let Some(req_bytes) = args.request_log_bytes {
        log_body(&args.telemetry, &args.call_id, BodyKind::Request, &req_bytes);
    }
    if let Some(resp_bytes) = response_log_bytes {
        log_body(&args.telemetry, &args.call_id, BodyKind::Response, &resp_bytes);
    }
}

fn strip_hop_by_hop(headers: &HeaderMap) -> HeaderMap {
    let mut out = headers.clone();
    for name in HOP_BY_HOP {
        out.remove(*name);
    }
    out
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn build_upstream_uri(upstream: &str, rest: &str, query: &str) -> Option<http::Uri> {
    format!("{upstream}{rest}{query}").parse().ok()
}

fn host_header_value(uri: &http::Uri) -> Option<HeaderValue> {
    let host = uri.host()?;
    let value = match uri.port_u16() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    HeaderValue::from_str(&value).ok()
}

fn log_record(telemetry: &Writer, record: &CallRecord) {
    if let Err(err) = telemetry.write(record) {
        tracing::warn!(error = %err, "failed to write call record");
    }
}

fn log_body(telemetry: &Writer, call_id: &str, kind: BodyKind, bytes: &[u8]) {
    if let Err(err) = telemetry.write_body(call_id, kind, bytes) {
        tracing::warn!(error = %err, "failed to write call body");
    }
}

fn not_found() -> Response<ResponseBody> {
    json_response(
        StatusCode::NOT_FOUND,
        serde_json::json!({"error": {"type": "ashkelon_route", "message": "unknown route"}})
            .to_string()
            .into_bytes(),
    )
}

fn bad_gateway(message: &str) -> Response<ResponseBody> {
    json_response(
        StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": {"type": "ashkelon_relay", "message": message}}).to_string().into_bytes(),
    )
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<ResponseBody> {
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(BoxBody::new(Full::new(Bytes::from(body))))
        .unwrap()
}
