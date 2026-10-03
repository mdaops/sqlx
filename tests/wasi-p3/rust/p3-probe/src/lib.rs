mod hardening;

use http_body_util::Full;
use wasip3::exports::http::handler::Guest;
use wasip3::http::types::{ErrorCode, Request, Response};
use wasip3::http_compat::http_into_wasi_response;

static INSTANCE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Component;
impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_default();
        if path == "/health" {
            return http_into_wasi_response(http::Response::new(Full::new(
                bytes::Bytes::from_static(b"p3-ready"),
            )));
        }
        if path == "/instance" {
            let id = INSTANCE.get_or_init(wasip3::random::random::get_random_u64);
            let calls = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            return http_into_wasi_response(http::Response::new(Full::new(bytes::Bytes::from(
                format!("instance={id} calls={calls}"),
            ))));
        }
        if matches!(
            path.as_str(),
            "/reuse" | "/reuse-seaorm" | "/tls" | "/tls-bad-ca" | "/cancel" | "/close-pool"
        ) {
            let result = hardening::run(&path).await;
            let (status, body) = match result {
                Ok(body) => (200, body),
                Err(e) => (500, format!("{e:#}")),
            };
            return http_into_wasi_response(
                http::Response::builder()
                    .status(status)
                    .body(Full::new(bytes::Bytes::from(body)))
                    .unwrap(),
            );
        }
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
        let result = match path.as_str() {
            "/sqlx" => sqlx_probe::run(&url).await,
            "/seaorm" => seaorm_probe::run(&url).await,
            _ => {
                return http_into_wasi_response(
                    http::Response::builder()
                        .status(404)
                        .body(Full::new(bytes::Bytes::from_static(b"unknown probe")))
                        .unwrap(),
                );
            }
        };
        let (status, body) = match result {
            Ok(()) => (
                200,
                format!("PASS {path}: typed insert/get, NULL, rollback via P3 sockets"),
            ),
            Err(e) => (500, format!("{e:#}")),
        };
        http_into_wasi_response(
            http::Response::builder()
                .status(status)
                .body(Full::new(bytes::Bytes::from(body)))
                .unwrap(),
        )
    }
}
wasip3::http::service::export!(Component);
