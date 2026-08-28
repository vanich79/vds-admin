//! The HTTPS listener.
//!
//! Three endpoints, and no more:
//!
//! | path | auth | why |
//! |---|---|---|
//! | `/v1/health` | none | liveness for systemd and load balancers; reveals only that an agent is here, which the open port already gave away |
//! | `/v1/info` | bearer | hostname, architecture and protocol version — enough to fingerprint the host, so it is behind the token |
//! | `/v1/metrics` | bearer | the full reading |
//!
//! Everything else is a 404. The agent serves no files, accepts no writes and runs no
//! commands on request: it reads the host and hands the result over. That is a deliberate
//! ceiling on what a stolen token is worth, and it is why the first version has no
//! "restart this service" endpoint even though the UI is built to grow one.
//!
//! TLS is terminated here with `tokio-rustls`, and each accepted connection is handed to
//! hyper on its own task. A handshake that fails — a probe, a scanner, a client with the
//! wrong pin — is logged at debug and dropped; it must never take the listener down.

use crate::auth::authorize;
use crate::collect::Collector;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use vds_agent_protocol::{
    AUTH_HEADER, AgentInfo, ErrorResponse, HealthResponse, PATH_HEALTH, PATH_INFO, PATH_METRICS,
    PROTOCOL_VERSION,
};

/// Everything the handlers need.
pub struct AgentState {
    pub collector: Collector,
    pub token: String,
    pub started_at: Instant,
    pub hostname: String,
}

impl AgentState {
    fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

/// Builds the router.
pub fn router(state: Arc<AgentState>) -> Router {
    Router::new()
        .route(PATH_HEALTH, get(health))
        .route(PATH_INFO, get(info))
        .route(PATH_METRICS, get(metrics))
        .fallback(not_found)
        .with_state(state)
}

/// Liveness. Unauthenticated by design; see the module documentation.
async fn health() -> Response {
    axum::Json(HealthResponse {
        ok: true,
        protocol_version: PROTOCOL_VERSION,
    })
    .into_response()
}

async fn info(State(state): State<Arc<AgentState>>, headers: HeaderMap) -> Response {
    if let Some(refusal) = check(&headers, &state, PATH_INFO) {
        return refusal;
    }

    axum::Json(AgentInfo {
        protocol_version: PROTOCOL_VERSION,
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        hostname: state.hostname.clone(),
        architecture: TARGET_TRIPLE.to_owned(),
        agent_uptime_secs: state.uptime_secs(),
        capabilities: crate::report::capabilities(),
    })
    .into_response()
}

async fn metrics(State(state): State<Arc<AgentState>>, headers: HeaderMap) -> Response {
    if let Some(refusal) = check(&headers, &state, PATH_METRICS) {
        return refusal;
    }

    match state.collector.report().await {
        Ok(report) => axum::Json(&*report).into_response(),
        Err(err) => {
            // The host itself is unreadable — a broken /proc, an exhausted process table.
            // 503 rather than 500: it is a condition that may pass.
            tracing::warn!(error = %err, "collection failed");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "collection failed",
                Some(err.to_string()),
            )
        }
    }
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "no such endpoint", None)
}

/// Authorises a request, returning the response to send when it fails.
///
/// `Option` rather than `Result`: an `Err` carrying a whole `Response` is a large error
/// type to move through every handler, and a failed authorisation is not exceptional
/// here — it is most of the traffic an internet-facing agent sees.
fn check(headers: &HeaderMap, state: &AgentState, path: &str) -> Option<Response> {
    let presented = headers
        .get(AUTH_HEADER)
        .and_then(|value| value.to_str().ok());

    match authorize(presented, &state.token) {
        Ok(()) => None,
        Err(failure) => {
            // The reason goes to the log; the client is told only "unauthorized".
            tracing::warn!(path, reason = failure.detail(), "rejected a request");
            Some(error_response(
                StatusCode::UNAUTHORIZED,
                failure.message(),
                None,
            ))
        }
    }
}

fn error_response(status: StatusCode, error: &str, detail: Option<String>) -> Response {
    (
        status,
        axum::Json(ErrorResponse {
            error: error.to_owned(),
            detail,
        }),
    )
        .into_response()
}

/// The triple this binary was built for, reported by `/v1/info`.
const TARGET_TRIPLE: &str = env!("VDS_AGENT_TARGET");

/// Accepts TLS connections until cancelled.
///
/// Returns once the listener is closed and no new connection will be accepted; in-flight
/// requests are given a moment to finish by the caller.
pub async fn serve(
    address: SocketAddr,
    tls: rustls::ServerConfig,
    router: Router,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = tokio::net::TcpListener::bind(address).await?;

    tracing::info!(%address, "listening");

    loop {
        let (stream, peer) = tokio::select! {
            // Cancellation is checked first so that a shutdown during a burst of
            // connections is not starved by the accept loop.
            biased;
            () = shutdown.cancelled() => {
                tracing::info!("shutting down the listener");
                return Ok(());
            }
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    // A per-connection accept error (a file-descriptor limit, a client
                    // that vanished mid-handshake) must not end the daemon.
                    tracing::warn!(error = %err, "could not accept a connection");
                    continue;
                }
            },
        };

        let acceptor = acceptor.clone();
        let router = router.clone();
        let shutdown = shutdown.clone();

        tokio::spawn(async move {
            let stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(err) => {
                    // Scanners and mispinned clients land here constantly. Debug, not warn.
                    tracing::debug!(%peer, error = %err, "TLS handshake failed");
                    return;
                }
            };

            let service = hyper::service::service_fn(move |request| {
                let router = router.clone();
                async move {
                    use tower::ServiceExt as _;
                    router.oneshot(request).await
                }
            });

            let connection = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service);
            tokio::pin!(connection);

            tokio::select! {
                result = connection.as_mut() => {
                    if let Err(err) = result {
                        tracing::debug!(%peer, error = %err, "connection ended with an error");
                    }
                }
                () = shutdown.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    let _ = connection.await;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;
    use vds_agent_protocol::{MetricsReport, bearer};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn state() -> Arc<AgentState> {
        Arc::new(AgentState {
            collector: Collector::new(&AgentConfig::default()),
            token: TOKEN.to_owned(),
            started_at: Instant::now(),
            hostname: "web-01".to_owned(),
        })
    }

    async fn call(path: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::builder().uri(path);
        if let Some(token) = token {
            request = request.header(AUTH_HEADER, bearer(token));
        }
        let request = request.body(Body::empty()).unwrap_or_default();

        let response = router(state())
            .oneshot(request)
            .await
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());

        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap_or_default();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        // systemd and load balancers must be able to check liveness without a secret.
        let (status, body) = call(PATH_HEALTH, None).await;
        assert_eq!(status, StatusCode::OK);

        let parsed: HealthResponse = serde_json::from_str(&body).expect("parses");
        assert!(parsed.ok);
        assert_eq!(parsed.protocol_version, PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn metrics_without_a_token_is_refused() {
        let (status, _) = call(PATH_METRICS, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn metrics_with_the_wrong_token_is_refused() {
        let (status, body) = call(PATH_METRICS, Some("wrong-0123456789abcdef0123456789")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // And the reply says nothing about *why*, so a prober learns nothing.
        assert!(!body.contains("does not match"), "body was: {body}");
    }

    #[tokio::test]
    async fn info_is_behind_the_token_too() {
        // Hostname and architecture are enough to fingerprint a host.
        assert_eq!(call(PATH_INFO, None).await.0, StatusCode::UNAUTHORIZED);

        let (status, body) = call(PATH_INFO, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK);

        let parsed: AgentInfo = serde_json::from_str(&body).expect("parses");
        assert_eq!(parsed.hostname, "web-01");
        assert_eq!(parsed.agent_version, env!("CARGO_PKG_VERSION"));
        assert!(!parsed.architecture.is_empty());
        assert!(parsed.capabilities.contains(&"docker".to_owned()));
    }

    #[tokio::test]
    async fn a_valid_token_gets_a_report_the_app_can_parse() {
        let (status, body) = call(PATH_METRICS, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK);

        let parsed: MetricsReport = serde_json::from_str(&body).expect("parses as a report");
        assert_eq!(parsed.protocol_version, PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn an_unknown_path_is_a_404_with_a_json_body() {
        // Not an HTML error page: every response the app can receive is JSON.
        let (status, body) = call("/v1/secrets", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let parsed: ErrorResponse = serde_json::from_str(&body).expect("parses");
        assert_eq!(parsed.error, "no such endpoint");
    }

    #[tokio::test]
    async fn an_unknown_path_is_a_404_even_without_a_token() {
        // The 404 must not become an oracle for which paths exist.
        assert_eq!(call("/v1/secrets", None).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_basic_auth_header_is_not_mistaken_for_a_bearer_token() {
        let request = Request::builder()
            .uri(PATH_METRICS)
            .header(AUTH_HEADER, format!("Basic {TOKEN}"))
            .body(Body::empty())
            .unwrap_or_default();

        let response = router(state())
            .oneshot(request)
            .await
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_agent_exposes_no_write_endpoints() {
        // A stolen token must be worth a reading of the host and nothing more.
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let request = Request::builder()
                .method(method)
                .uri(PATH_METRICS)
                .header(AUTH_HEADER, bearer(TOKEN))
                .body(Body::empty())
                .unwrap_or_default();

            let response = router(state())
                .oneshot(request)
                .await
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());

            assert_ne!(
                response.status(),
                StatusCode::OK,
                "{method} was accepted on the metrics endpoint"
            );
        }
    }

    #[tokio::test]
    async fn the_reported_uptime_is_the_agents_own() {
        let state = state();
        let request = Request::builder()
            .uri(PATH_INFO)
            .header(AUTH_HEADER, bearer(TOKEN))
            .body(Body::empty())
            .unwrap_or_default();

        let response = router(Arc::clone(&state))
            .oneshot(request)
            .await
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap_or_default();
        let parsed: AgentInfo = serde_json::from_slice(&bytes).expect("parses");

        // Freshly started, so this is the host's uptime only by coincidence — and it
        // must not be: the two are different numbers and the UI shows both.
        assert!(parsed.agent_uptime_secs < 5);
    }
}
