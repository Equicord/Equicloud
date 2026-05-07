use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderValue;
use dotenv::dotenv;
use equicloud::constants::{DB_HEALTH_CHECK_INTERVAL_SECS, DEFAULT_HOST, DEFAULT_PORT};
use equicloud::utils::CONFIG;
use equicloud::{DatabaseService, MigrationRunner, create_database_connection};
use governor::middleware::NoOpMiddleware;
use http::Method;
use http::header::{CONTENT_TYPE, HeaderName};
use std::env;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::{PeerIpKeyExtractor, SmartIpKeyExtractor};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::{error, info, warn};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod middleware;
mod routes;

type SecurityHeaderLayer =
    SetResponseHeaderLayer<fn(&http::Response<axum::body::Body>) -> Option<HeaderValue>>;

fn configure_cors() -> CorsLayer {
    let origins = env::var("CORS_ALLOWED_ORIGINS").ok();

    match origins.as_deref() {
        Some("*") => {
            warn!(
                "CORS_ALLOWED_ORIGINS=*; allowing all origins. This is unsafe for production deployments."
            );
            CorsLayer::permissive()
        }
        Some(origins_str) => {
            let valid_origins: Vec<HeaderValue> = origins_str
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();

            if valid_origins.is_empty() {
                warn!("No valid CORS origins parsed, CORS will reject cross-origin requests");
                CorsLayer::new()
            } else {
                info!("CORS configured for {} origins", valid_origins.len());
                CorsLayer::new()
                    .allow_origin(valid_origins)
                    .allow_methods([
                        Method::GET,
                        Method::POST,
                        Method::PUT,
                        Method::DELETE,
                        Method::HEAD,
                        Method::OPTIONS,
                    ])
                    .allow_headers([
                        CONTENT_TYPE,
                        HeaderName::from_static("authorization"),
                        HeaderName::from_static("if-none-match"),
                        HeaderName::from_static("if-match"),
                    ])
                    .expose_headers([
                        HeaderName::from_static("etag"),
                        HeaderName::from_static("x-version"),
                    ])
            }
        }
        None => {
            warn!(
                "CORS_ALLOWED_ORIGINS is not set; cross-origin requests will be rejected. \
                 Set to a comma-separated origin list, or `*` for development only."
            );
            CorsLayer::new()
        }
    }
}

fn build_rate_limiter_peer_config()
-> tower_governor::governor::GovernorConfig<PeerIpKeyExtractor, NoOpMiddleware> {
    let (per_second, burst_size) = rate_limit_params();
    GovernorConfigBuilder::default()
        .per_second(per_second)
        .burst_size(burst_size)
        .finish()
        .expect("Failed to build rate limiter config")
}

fn build_rate_limiter_proxy_config()
-> tower_governor::governor::GovernorConfig<SmartIpKeyExtractor, NoOpMiddleware> {
    let (per_second, burst_size) = rate_limit_params();
    GovernorConfigBuilder::default()
        .per_second(per_second)
        .burst_size(burst_size)
        .key_extractor(SmartIpKeyExtractor)
        .finish()
        .expect("Failed to build rate limiter config")
}

fn rate_limit_params() -> (u64, u32) {
    let per_second: u64 = parse_env("RATE_LIMIT_PER_SECOND", 50);
    let burst_size: u32 = parse_env("RATE_LIMIT_BURST", 150);

    info!("Rate limiting: {} req/s, burst: {}", per_second, burst_size);

    (per_second, burst_size)
}

fn parse_env<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(v) if !v.is_empty() => match v.parse() {
            Ok(parsed) => parsed,
            Err(e) => {
                error!(
                    "Invalid value for {}={:?}: {}; falling back to default",
                    name, v, e
                );
                default
            }
        },
        _ => default,
    }
}

fn security_headers_layer() -> SecurityHeaderLayer {
    SetResponseHeaderLayer::overriding(
        HeaderName::from_static("x-content-type-options"),
        |_res: &http::Response<axum::body::Body>| Some(HeaderValue::from_static("nosniff")),
    )
}

fn frame_options_layer() -> SecurityHeaderLayer {
    SetResponseHeaderLayer::overriding(
        HeaderName::from_static("x-frame-options"),
        |_res: &http::Response<axum::body::Body>| Some(HeaderValue::from_static("DENY")),
    )
}

fn cache_control_layer() -> SecurityHeaderLayer {
    SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static("cache-control"),
        |_res: &http::Response<axum::body::Body>| {
            Some(HeaderValue::from_static("no-store, max-age=0"))
        },
    )
}

fn referrer_policy_layer() -> SecurityHeaderLayer {
    SetResponseHeaderLayer::overriding(
        HeaderName::from_static("referrer-policy"),
        |_res: &http::Response<axum::body::Body>| Some(HeaderValue::from_static("no-referrer")),
    )
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting EquiCloud server");
    info!("Connecting to database...");

    let session = match create_database_connection().await {
        Ok(session) => {
            info!("Database connection successful");
            session
        }
        Err(e) => {
            error!("Failed to connect to database: {}", e);
            std::process::exit(1);
        }
    };

    info!("Running migrations...");
    let migration_runner = MigrationRunner::new(&session);
    if let Err(e) = migration_runner.run_migrations().await {
        error!("Failed to run migrations: {}", e);
        std::process::exit(1);
    }
    info!("Migrations completed");

    let db_service = match DatabaseService::new(session).await {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to create database service: {}", e);
            std::process::exit(1);
        }
    };

    let server_host = env::var("SERVER_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let server_port = env::var("SERVER_PORT").unwrap_or_else(|_| DEFAULT_PORT.to_string());
    let bind_address = format!("{}:{}", server_host, server_port);

    let rate_limit_enabled: bool = parse_env("RATE_LIMIT_ENABLED", true);
    let trust_proxy_headers: bool = parse_env("TRUST_PROXY_HEADERS", false);

    let cors = configure_cors();

    let max_body_size = CONFIG.max_backup_size_bytes.saturating_add(4096);

    let internal_routes = Router::new()
        .merge(routes::health::register())
        .merge(routes::metrics::register())
        .layer(axum::extract::Extension(db_service.clone()))
        // Read-only routes — tight 8 KiB body cap, can never legitimately have a body.
        .layer(DefaultBodyLimit::max(8 * 1024));

    let api_routes = Router::new()
        .merge(routes::v1::register())
        .merge(routes::v2::register())
        .layer(axum::extract::Extension(db_service.clone()))
        // Bulk-upload routes (PUT settings, PUT data, POST sync) need the
        // configured backup size + a small slack for headers/JSON wrapping.
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(max_body_size));

    enum LimiterCleanup {
        Peer(
            std::sync::Arc<
                governor::RateLimiter<
                    std::net::IpAddr,
                    governor::state::keyed::DashMapStateStore<std::net::IpAddr>,
                    governor::clock::QuantaClock,
                    NoOpMiddleware,
                >,
            >,
        ),
        Proxy(
            std::sync::Arc<
                governor::RateLimiter<
                    std::net::IpAddr,
                    governor::state::keyed::DashMapStateStore<std::net::IpAddr>,
                    governor::clock::QuantaClock,
                    NoOpMiddleware,
                >,
            >,
        ),
        None,
    }

    let (api_routes, cleanup) = match (rate_limit_enabled, trust_proxy_headers) {
        (true, true) => {
            info!("Rate limiting enabled (trusting proxy headers)");
            let cfg = build_rate_limiter_proxy_config();
            let limiter = cfg.limiter().clone();
            (
                api_routes.layer(GovernorLayer::new(cfg)),
                LimiterCleanup::Proxy(limiter),
            )
        }
        (true, false) => {
            info!("Rate limiting enabled (using peer IP)");
            let cfg = build_rate_limiter_peer_config();
            let limiter = cfg.limiter().clone();
            (
                api_routes.layer(GovernorLayer::new(cfg)),
                LimiterCleanup::Peer(limiter),
            )
        }
        (false, _) => {
            warn!("Rate limiting disabled");
            (api_routes, LimiterCleanup::None)
        }
    };

    // Periodically prune the governor's per-IP map to prevent unbounded growth.
    match cleanup {
        LimiterCleanup::Peer(limiter) | LimiterCleanup::Proxy(limiter) => {
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                interval.tick().await; // skip the immediate first tick
                loop {
                    interval.tick().await;
                    limiter.retain_recent();
                }
            });
        }
        LimiterCleanup::None => {}
    }

    let app = Router::new()
        .merge(internal_routes)
        .merge(api_routes)
        // Outermost: response-shape headers, applied to every response.
        .layer(cors)
        .layer(security_headers_layer())
        .layer(frame_options_layer())
        .layer(cache_control_layer())
        .layer(referrer_policy_layer());

    let listener = TcpListener::bind(&bind_address).await.unwrap_or_else(|e| {
        error!("Failed to bind to address {}: {}", bind_address, e);
        std::process::exit(1);
    });

    info!("Server running on http://{}", bind_address);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<&'static str>();

    let health_check_db = db_service.clone();
    tokio::spawn(async move {
        let mut consecutive_failures = 0;
        const MAX_CONSECUTIVE_FAILURES: u32 = 3;

        loop {
            tokio::time::sleep(Duration::from_secs(DB_HEALTH_CHECK_INTERVAL_SECS)).await;

            match health_check_db.health_check().await {
                Ok(_) => {
                    if consecutive_failures > 0 {
                        info!("Database connection restored");
                        consecutive_failures = 0;
                    }
                }
                Err(e) => {
                    consecutive_failures += 1;
                    error!(
                        "Database health check failed ({}/{}): {}",
                        consecutive_failures, MAX_CONSECUTIVE_FAILURES, e
                    );

                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        error!(
                            "Database connection lost after {} consecutive failures, shutting down",
                            MAX_CONSECUTIVE_FAILURES
                        );
                        let _ = shutdown_tx.send("database unhealthy");
                        return;
                    }
                }
            }
        }
    });

    let shutdown_signal = async move {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
            "ctrl-c"
        };
        #[cfg(unix)]
        let terminate = async {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut sig) = signal(SignalKind::terminate()) {
                sig.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
            "SIGTERM"
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<&'static str>();

        let reason = tokio::select! {
            r = ctrl_c => r,
            r = terminate => r,
            r = shutdown_rx => r.unwrap_or("shutdown channel closed"),
        };
        info!("Shutdown signal received: {}; draining in-flight requests", reason);
    };

    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal)
    .await;

    match serve_result {
        Ok(()) => {
            info!("Server shut down cleanly");
        }
        Err(e) => {
            error!("Server failed: {}", e);
            std::process::exit(1);
        }
    }
}
