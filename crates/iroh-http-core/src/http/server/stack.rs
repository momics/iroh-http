//! Tower stack composition shared by [`crate::http::serve`] (server) and
//! [`crate::http::client::fetch_request`] (client).
//!
//! Closes Slice B of #182.
//!
//! Before this slice the per-connection pipeline was assembled by
//! [`super::pipeline::build_stack`] from a hand-coded
//! `PipelineParams` struct, while the outgoing-fetch pipeline was assembled
//! inline in `client.rs` from a one-off `HyperClientSvc` wrapper.
//! Compression on serve and decompression on fetch composed with different
//! code paths to do structurally the same thing.
//!
//! After this slice there is exactly one composition function per direction
//! ([`build_stack`] for inbound, [`build_client_stack`] for outbound),
//! both consume the same typed [`StackConfig`], and every middleware is
//! driven by a dedicated `apply_*` factory taking [`ServeService`] →
//! [`ServeService`] (axum-style boxed-per-layer composition). Toggling
//! a `cfg` field off produces a service whose runtime shape is identical
//! to the layer never having been built (no-op early-return).
//!
//! Layer ordering (outermost first), inbound:
//!
//! ```text
//! [body limit →] HandleLayerError → [load shed →] [timeout →]
//!   [compression →] [decompression →] svc
//! ```
//!
//! Layer ordering (outermost first), outbound:
//!
//! ```text
//! [decompression →] hyper SendRequest
//! ```
//!
//! ## Why two functions, one config
//!
//! Server and client use the same primitive layers from `tower-http`, but
//! in opposite roles: a server *responds* compressed (compression layer)
//! and *accepts* compressed requests (decompression layer); a client *sends*
//! plain and *accepts* compressed responses (decompression layer). A single
//! [`StackConfig`] captures the operator's intent; the direction-specific
//! function decides which subset to apply.
//!
//! Slice D (#186) replaces [`build_client_stack`]'s ad-hoc hyper wrapper
//! with a proper `Service<Request<Body>, Response<Body>, FetchError>`
//! contract; until then, the wrapper lives here so the construction site
//! is unified even if the inner type still spells `hyper::Error`.

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use tower::ServiceBuilder;

use crate::http::body::BoxError;
use crate::Body;

// ── Public-facing config type ────────────────────────────────────────────────

/// Compression options for response bodies (server) and outgoing requests
/// (client; reserved).
///
/// Lives in this module because it configures the compression layer in
/// [`build_stack`]/[`build_client_stack`]. Re-exported from the crate root
/// so adapter callsites are unchanged.
#[derive(Debug, Clone)]
pub struct CompressionOptions {
    /// Minimum body size in bytes before compression is applied.
    /// Default: [`CompressionOptions::DEFAULT_MIN_BODY_BYTES`] (1 KiB).
    pub min_body_bytes: usize,
    /// Zstd compression level (1–22). `None` uses the zstd default (3).
    pub level: Option<u32>,
}

impl CompressionOptions {
    /// Default minimum body size before compression is applied.
    ///
    /// 1 KiB. Matches the documented default and the threshold most HTTP
    /// servers tune to: below ~1 KiB the CPU cost of compression typically
    /// outweighs the bandwidth savings on a single TCP/QUIC packet.
    pub const DEFAULT_MIN_BODY_BYTES: usize = 1024;
}

impl Default for CompressionOptions {
    fn default() -> Self {
        Self {
            min_body_bytes: Self::DEFAULT_MIN_BODY_BYTES,
            level: None,
        }
    }
}

// ── Internal stack types ─────────────────────────────────────────────────────

/// Server-side stack alias: type-erased `Service<Request<Body>>` returning
/// `Response<Body>` with [`Infallible`] errors.
///
/// Boxing once at the construction site (in `server::serve_with_events`)
/// means every future addition to the layer stack — `AddExtensionLayer`,
/// `TraceLayer`, a response-signing layer, anything — is **one append**
/// in the builder without rippling a new concrete type signature through
/// the per-bistream code path.
pub(crate) type ServeService =
    tower::util::BoxCloneService<hyper::Request<Body>, hyper::Response<Body>, Infallible>;

/// Client-side stack alias.
///
/// Carries `hyper::Error` (Slice D will replace this with a typed
/// `FetchError`). Boxed via [`tower::util::BoxService`] rather than
/// [`tower::util::BoxCloneService`] because hyper's `SendRequest` is not
/// `Clone`; the boxed service is consumed by a single `oneshot` call.
pub(crate) type ClientService =
    tower::util::BoxService<hyper::Request<Body>, hyper::Response<Body>, hyper::Error>;

/// Tower stack configuration shared by [`build_stack`] and
/// [`build_client_stack`].
///
/// Each field is consumed by a dedicated `apply_*` factory in this module
/// (see [`build_stack`]'s body). Factories are no-ops when their field is
/// `None`/`false`, so toggling a field off produces a service whose
/// runtime shape is identical to the layer never having been built.
///
/// Composition uses the axum-style boxed-per-layer pattern: every
/// `apply_*` takes a [`ServeService`] and returns a [`ServeService`],
/// performing its own `boxed_clone()`. This re-erases the inner
/// `Service::Future` after each layer, so layer addition is decoupled
/// from the type-soup of `Either<L::Service, S>` and `impl Layer<…>`
/// return-position bound erasure (see Slice B (#184) carry-forward in
/// Slice C (#185)).
///
/// Slice D (#186): made public so the pure-Rust
/// [`crate::http::client::fetch_request`] can take `&StackConfig` directly.
/// `#[non_exhaustive]` so adapters constructing literal `StackConfig`
/// values (`StackConfig { timeout, decompression, ..default() }`) opt in
/// explicitly and future fields don't break them.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct StackConfig {
    /// Per-request timeout. `None` ⇒ no `TimeoutLayer` is applied.
    pub timeout: Option<Duration>,
    /// Maximum **wire** request body size before the server rejects with 413.
    ///
    /// This limit is applied **before** decompression, i.e. it counts compressed
    /// bytes as they arrive from the QUIC stream. It is an effective guard
    /// against network-level DoS (high-bandwidth senders), but it does **not**
    /// protect against a compression bomb (a small compressed body that
    /// decompresses to many GB). Use [`Self::max_request_body_decoded_bytes`]
    /// for that.
    pub max_request_body_wire_bytes: Option<usize>,
    /// Maximum **decoded** request body size before the server rejects with 413.
    ///
    /// This limit is applied **after** decompression. It is the primary guard
    /// against compression bombs: a zstd payload that is tiny on the wire but
    /// expands to GB in memory is rejected once the decoded byte count crosses
    /// this threshold. The default at the [`super::serve`] entry point
    /// is 16 MiB (matching the documented behaviour that the old single-limit
    /// field `max_request_body_bytes` had always promised but never delivered).
    pub max_request_body_decoded_bytes: Option<usize>,
    /// `true` ⇒ wrap with `LoadShedLayer` so saturated capacity returns 503
    /// immediately rather than blocking the caller.
    pub load_shed: bool,
    /// Operator's compression configuration. `None` disables response
    /// compression on the server side; ignored by [`build_client_stack`]
    /// (clients do not yet compress request bodies).
    pub compression: Option<CompressionOptions>,
    /// `true` ⇒ apply `RequestDecompressionLayer` (server) /
    /// `DecompressionLayer` (client). Defaults to `true` — every server
    /// today accepts compressed requests, every client accepts compressed
    /// responses.
    pub decompression: bool,
}

impl Default for StackConfig {
    fn default() -> Self {
        Self {
            timeout: None,
            max_request_body_wire_bytes: None,
            max_request_body_decoded_bytes: None,
            load_shed: false,
            compression: None,
            decompression: true,
        }
    }
}

impl StackConfig {
    /// Set [`Self::timeout`] using the builder pattern.
    ///
    /// Convenience constructor for external callers — `#[non_exhaustive]`
    /// blocks struct-literal construction outside this crate, so use
    /// `StackConfig::default().with_timeout(...)`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set [`Self::decompression`] using the builder pattern.
    #[must_use]
    pub fn with_decompression(mut self, decompression: bool) -> Self {
        self.decompression = decompression;
        self
    }
}

// ── Server pipeline ──────────────────────────────────────────────────────────

/// Compose the inbound per-connection tower stack.
///
/// Layer ordering (outermost first):
///
/// ```text
///   wire body limit  →  load shed  →  request timeout
///                    →  compression (response)
///                    →  decompression (request)
///                    →  decoded body limit
///                    →  svc
/// ```
///
/// Built bottom-up (innermost first) by chaining `apply_*` factories,
/// each of which takes a [`ServeService`] and returns a [`ServeService`].
/// Adding a new layer is a single line.
///
/// The two body-limit layers enforce different semantic guarantees:
/// - **wire limit** (outermost): counts compressed bytes from the QUIC
///   stream. Guards against high-bandwidth network floods.
/// - **decoded limit** (inside decompression): counts bytes after
///   `RequestDecompressionLayer` expands them. Guards against zstd/gzip
///   compression bombs (#190).
///
/// `HandleLayerError` is owned **by each layer that can produce a
/// `BoxError`** (load-shed, timeout) rather than threaded as a separate
/// outer wrap: this keeps every factory's signature uniformly
/// `ServeService → ServeService` and the chain composable in any order.
/// See Slice C carry-forward (#185) for the design rationale.
pub(crate) fn build_stack(svc: ServeService, cfg: &StackConfig) -> ServeService {
    let svc = apply_body_limit(svc, cfg.max_request_body_decoded_bytes); // inside decompression
    let svc = apply_decompression(svc, cfg.decompression);
    let svc = apply_compression(svc, cfg.compression.as_ref());
    let svc = apply_timeout(svc, cfg.timeout);
    let svc = apply_load_shed(svc, cfg.load_shed);
    apply_body_limit(svc, cfg.max_request_body_wire_bytes) // outermost: wire bytes
}

// ── Renormalisation helper ───────────────────────────────────────────────────

/// Convert any body type back to [`Body`].
///
/// Used as the mapping function in `MapResponseBodyLayer` / `MapRequestBodyLayer`
/// after every layer that changes the body type. Since [`Body::new`] already
/// short-circuits via `try_downcast` when the input is already a `Body`, the
/// cost of using this at every seam is essentially free for pass-through layers.
fn renormalize_body<B>(body: B) -> Body
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    Body::new(body)
}

// ── Layer factories ──────────────────────────────────────────────────────────

/// `RequestBodyLimitLayer` rejects oversize bodies with 413. Wrapped in
/// matching `MapRequestBody` / `MapResponseBody` to renormalise both
/// directions back to [`Body`] (ADR-014 D2 / #175).
pub(crate) fn apply_body_limit(svc: ServeService, limit: Option<usize>) -> ServeService {
    use tower::ServiceExt;
    use tower_http::limit::RequestBodyLimitLayer;
    use tower_http::map_request_body::MapRequestBodyLayer;
    use tower_http::map_response_body::MapResponseBodyLayer;

    let Some(limit) = limit else {
        return svc;
    };
    ServiceBuilder::new()
        .layer(MapResponseBodyLayer::new(renormalize_body))
        .layer(RequestBodyLimitLayer::new(limit))
        .layer(MapRequestBodyLayer::new(renormalize_body))
        .service(svc)
        .boxed_clone()
}

/// `LoadShedLayer` returns `Overloaded` immediately when the inner
/// service reports `Poll::Pending` from `poll_ready`. The error is
/// converted to a 503 response by an inner [`HandleLayerErrorLayer`] so
/// the resulting service still has `Error = Infallible`. No-op when
/// `enabled = false`.
pub(crate) fn apply_load_shed(svc: ServeService, enabled: bool) -> ServeService {
    use crate::http::server::HandleLayerErrorLayer;
    use tower::ServiceExt;
    if !enabled {
        return svc;
    }
    ServiceBuilder::new()
        .layer(HandleLayerErrorLayer)
        .layer(tower::load_shed::LoadShedLayer::new())
        .service(svc)
        .boxed_clone()
}

/// Per-request timeout. The `Elapsed` error is converted to a 408
/// response by an inner [`HandleLayerErrorLayer`] so the resulting
/// service still has `Error = Infallible`. No-op when `timeout = None`.
pub(crate) fn apply_timeout(svc: ServeService, timeout: Option<Duration>) -> ServeService {
    use crate::http::server::HandleLayerErrorLayer;
    use tower::ServiceExt;
    let Some(timeout) = timeout else {
        return svc;
    };
    ServiceBuilder::new()
        .layer(HandleLayerErrorLayer)
        .layer(tower::timeout::TimeoutLayer::new(timeout))
        .service(svc)
        .boxed_clone()
}

/// Response compression with project-specific predicates (see
/// [`build_compression_layer`]). No-op when `comp = None`.
pub(crate) fn apply_compression(
    svc: ServeService,
    comp: Option<&CompressionOptions>,
) -> ServeService {
    use tower::ServiceExt;
    use tower_http::map_response_body::MapResponseBodyLayer;

    let Some(comp) = comp else {
        return svc;
    };
    ServiceBuilder::new()
        .layer(MapResponseBodyLayer::new(renormalize_body))
        .layer(build_compression_layer(comp))
        .service(svc)
        .boxed_clone()
}

/// Request decompression. Wrapped in matching `MapRequestBody` /
/// `MapResponseBody` to renormalise both body sides back to [`Body`].
/// No-op when `enabled = false`.
pub(crate) fn apply_decompression(svc: ServeService, enabled: bool) -> ServeService {
    use tower::ServiceExt;
    use tower_http::decompression::RequestDecompressionLayer;
    use tower_http::map_request_body::MapRequestBodyLayer;
    use tower_http::map_response_body::MapResponseBodyLayer;

    if !enabled {
        return svc;
    }
    ServiceBuilder::new()
        .layer(MapResponseBodyLayer::new(renormalize_body))
        .layer(RequestDecompressionLayer::new())
        .layer(MapRequestBodyLayer::new(renormalize_body))
        .service(svc)
        .boxed_clone()
}

/// Build the `tower-http` compression layer with the project's predicate set.
///
/// Two custom predicates remain because tower-http does not ship built-ins
/// for either:
///
/// 1. Skip if the response already carries `Content-Encoding` (handler
///    returned a pre-encoded body — re-compressing would double-encode).
/// 2. Honour `Cache-Control: no-transform` per RFC 9111 §5.2.2.7.
pub(crate) fn build_compression_layer(
    comp: &CompressionOptions,
) -> tower_http::compression::CompressionLayer<impl tower_http::compression::Predicate> {
    use http::{Extensions, HeaderMap, StatusCode, Version};
    use tower_http::compression::{
        predicate::{NotForContentType, Predicate, SizeAbove},
        CompressionLayer, CompressionLevel,
    };

    let mut layer = CompressionLayer::new().zstd(true);
    if let Some(level) = comp.level {
        layer = layer.quality(CompressionLevel::Precise(level as i32));
    }

    let not_pre_compressed = |_: StatusCode, _: Version, h: &HeaderMap, _: &Extensions| {
        !h.contains_key(http::header::CONTENT_ENCODING)
    };
    let not_no_transform = |_: StatusCode, _: Version, h: &HeaderMap, _: &Extensions| {
        h.get(http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                !v.split(',')
                    .any(|d| d.trim().eq_ignore_ascii_case("no-transform"))
            })
            .unwrap_or(true)
    };

    let predicate = SizeAbove::new(comp.min_body_bytes as u64)
        .and(NotForContentType::IMAGES)
        .and(NotForContentType::SSE)
        .and(NotForContentType::const_new("audio/"))
        .and(NotForContentType::const_new("video/"))
        .and(NotForContentType::const_new("application/zstd"))
        .and(NotForContentType::const_new("application/octet-stream"))
        .and(not_pre_compressed)
        .and(not_no_transform);

    layer.compress_when(predicate)
}

// ── Client pipeline ──────────────────────────────────────────────────────────

/// Compose the outbound per-request tower stack.
///
/// ```text
///   [timeout →] decompression → incoming→Body → hyper SendRequest
/// ```
///
/// Returns a [`ClientService`] — boxed once so the caller
/// ([`crate::http::client::fetch_request`]) does not have to spell the inner
/// type. Honours `cfg.decompression`. `cfg.timeout` is **not** wired into
/// this stack — enforcing it as a tower layer would change the service
/// error type away from `hyper::Error`. The pure-Rust caller
/// [`crate::http::client::fetch_request`] applies `cfg.timeout` with an
/// outer `tokio::time::timeout` instead. Per Slice D
/// (#186) the inner `SendRequest` wrapper that used to be a named
/// `HyperClientSvc` lives inline below.
pub(crate) fn build_client_stack(
    sender: hyper::client::conn::http1::SendRequest<Body>,
    cfg: &StackConfig,
) -> ClientService {
    use tower::ServiceExt;
    use tower_http::map_response_body::MapResponseBodyLayer;

    // Inline `SendRequest<Body> as tower::Service` adapter. Lives inside
    // `build_client_stack` so the wrapper has no name outside this
    // function (Slice D acceptance #3).
    struct SendRequestSvc(hyper::client::conn::http1::SendRequest<Body>);

    impl tower::Service<hyper::Request<Body>> for SendRequestSvc {
        type Response = hyper::Response<hyper::body::Incoming>;
        type Error = hyper::Error;
        type Future = std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
        >;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, req: hyper::Request<Body>) -> Self::Future {
            Box::pin(self.0.send_request(req))
        }
    }

    // Step 1: normalise hyper's `Incoming` to `Body` so subsequent layers
    // operate on the canonical body type.
    let svc = ServiceBuilder::new()
        .layer(MapResponseBodyLayer::new(renormalize_body))
        .service(SendRequestSvc(sender))
        .boxed();

    // Step 2: optional response decompression (always-on by default).
    apply_client_decompression(svc, cfg.decompression)
    // Per-request timeout is enforced by the caller in
    // [`crate::http::client::fetch_request`] via `tokio::time::timeout` rather
    // than a `TimeoutLayer` here, so the `ClientService` error type can
    // stay `hyper::Error` and the timeout maps cleanly to
    // `FetchError::Timeout` without going through `BoxError` downcasts.
}

/// Client-side equivalent of [`apply_decompression`]. No-op when
/// `enabled = false`.
pub(crate) fn apply_client_decompression(svc: ClientService, enabled: bool) -> ClientService {
    use tower::ServiceExt;
    use tower_http::decompression::DecompressionLayer;
    use tower_http::map_response_body::MapResponseBodyLayer;

    if !enabled {
        return svc;
    }
    ServiceBuilder::new()
        .layer(MapResponseBodyLayer::new(renormalize_body))
        .layer(DecompressionLayer::new())
        .service(svc)
        .boxed()
}

#[cfg(test)]
mod tests {
    //! ADR-014 D2 / #175 + Slice B (#184) guardrail.
    //!
    //! Exercises the *real* per-bistream chain ([`build_stack`]) to prove
    //! the structural property the issue was filed for: every layer in the
    //! production pipeline composes uniformly into [`ServeService`], and a
    //! request still flows through to a `Response<Body>`. If a future
    //! change to the inner service's body or error type breaks uniformity,
    //! `build_stack` itself stops compiling — the failure surfaces here,
    //! not later in an integration test over the network.
    //!
    //! No hyper / no Iroh / no networking: feeds requests directly into
    //! the boxed [`ServeService`] with `tower::ServiceExt::oneshot`.

    use super::*;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use std::convert::Infallible;
    use tower::ServiceExt;

    /// Stand-in for `IrohHttpService` shaped exactly like the real one
    /// (`Service<Request<Body>, Response = Response<Body>, Error = Infallible>`).
    /// Echoes the request body back in the response.
    #[derive(Clone)]
    struct EchoService;

    impl tower::Service<hyper::Request<Body>> for EchoService {
        type Response = hyper::Response<Body>;
        type Error = Infallible;
        type Future = std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
        >;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: hyper::Request<Body>) -> Self::Future {
            Box::pin(async move {
                let bytes = req
                    .into_body()
                    .collect()
                    .await
                    .map(|c| c.to_bytes())
                    .unwrap_or_default();
                Ok(hyper::Response::new(Body::full(bytes)))
            })
        }
    }

    fn default_cfg() -> StackConfig {
        StackConfig {
            timeout: None,
            max_request_body_wire_bytes: Some(1024 * 1024),
            max_request_body_decoded_bytes: Some(1024 * 1024),
            load_shed: true,
            compression: None,
            decompression: true,
        }
    }

    fn boxed_echo() -> ServeService {
        ServiceBuilder::new().service(EchoService).boxed_clone()
    }

    /// Drives the *production* `build_stack`. Body limit, load-shed and
    /// decompression are all in-circuit. If any of them stops composing
    /// into `ServeService`, this fails to compile — that is the structural
    /// guardrail.
    #[tokio::test]
    async fn real_chain_round_trips_a_request() {
        let stack = build_stack(boxed_echo(), &default_cfg());

        let req = hyper::Request::builder()
            .uri("/")
            .body(Body::full("ping"))
            .unwrap();
        let resp = stack.oneshot(req).await.expect("service infallible");
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("body collect")
            .to_bytes();
        assert_eq!(body, Bytes::from_static(b"ping"));
    }

    /// Same chain, with response-side compression enabled. Proves the
    /// `option_layer(comp_layer)` arm composes and produces the same
    /// `ServeService` shape.
    #[tokio::test]
    async fn real_chain_with_compression_enabled_still_round_trips() {
        let mut cfg = default_cfg();
        cfg.compression = Some(CompressionOptions {
            level: None,
            min_body_bytes: 0,
        });
        let stack = build_stack(boxed_echo(), &cfg);

        let req = hyper::Request::builder()
            .uri("/")
            .header("accept-encoding", "zstd")
            .body(Body::full("ping"))
            .unwrap();
        let resp = stack.oneshot(req).await.expect("service infallible");
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        // Body is opaque (may be zstd-encoded). We only assert structural
        // success; wire-format coverage lives in the dedicated compression
        // integration tests.
        let _ = resp.into_body().collect().await;
    }

    /// #184 acceptance criterion 5 — extensibility regression test.
    ///
    /// Wraps the production `build_stack` output with one additional
    /// `MapRequestBodyLayer::new(identity)` and asserts the request still
    /// flows. Demonstrates ADR-014 D2's structural payoff at runtime: a
    /// new layer is one append, no signature change anywhere downstream
    /// because both `build_stack`'s input and output are
    /// [`ServeService`]. If a future layer addition breaks the
    /// "uniformly composes into `ServeService`" property this test fails
    /// to compile.
    #[tokio::test]
    async fn build_stack_accepts_additional_outer_layer() {
        use tower_http::map_request_body::MapRequestBodyLayer;

        let inner = build_stack(boxed_echo(), &default_cfg());
        let stack = ServiceBuilder::new()
            .layer(MapRequestBodyLayer::new(|b: Body| b))
            .service(inner);

        let req = hyper::Request::builder()
            .uri("/")
            .body(Body::full("ping"))
            .unwrap();
        let resp = stack.oneshot(req).await.expect("service infallible");
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("body collect")
            .to_bytes();
        assert_eq!(body, Bytes::from_static(b"ping"));
    }
}
