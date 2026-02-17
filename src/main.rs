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

mod middleware;
mod routes;

type SecurityHeaderLayer =
    SetResponseHeaderLayer<fn(&http::Response<axum::body::Body>) -> Option<HeaderValue>>;

fn configure_cors() -> CorsLayer {
    let origins = env::var("CORS_ALLOWED_ORIGINS").ok();

    match origins.as_deref() {
        Some("*") => {
            warn!("CORS configured for all origins - use specific origins in production!");
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
                    ])
                    .expose_headers([
                        HeaderName::from_static("etag"),
                        HeaderName::from_static("x-version"),
                    ])
            }
        }
        None => {
            warn!("CORS_ALLOWED_ORIGINS not set - defaulting to permissive for development");
            CorsLayer::permissive()
        }
    }
}

fn configure_rate_limiter_peer() -> GovernorLayer<PeerIpKeyExtractor, NoOpMiddleware, axum::body::Body>
{
    let (per_second, burst_size) = rate_limit_params();

    let config = GovernorConfigBuilder::default()
        .per_second(per_second)
        .burst_size(burst_size)
        .finish()
        .expect("Failed to build rate limiter config");

    GovernorLayer::new(config)
}

fn configure_rate_limiter_proxy(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let (per_second, burst_size) = rate_limit_params();

    let config = GovernorConfigBuilder::default()
        .per_second(per_second)
        .burst_size(burst_size)
        .key_extractor(SmartIpKeyExtractor)
        .finish()
        .expect("Failed to build rate limiter config");

    GovernorLayer::new(config)
}

fn rate_limit_params() -> (u64, u32) {
    let per_second = env::var("RATE_LIMIT_PER_SECOND")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50u64);

    let burst_size = env::var("RATE_LIMIT_BURST")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150u32);

    info!("Rate limiting: {} req/s, burst: {}", per_second, burst_size);

    (per_second, burst_size)
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

    let rate_limit_enabled = env::var("RATE_LIMIT_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true);

    let trust_proxy_headers = env::var("TRUST_PROXY_HEADERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false);

    let cors = configure_cors();

    let max_body_size = CONFIG.max_backup_size_bytes + 4096;

    let app = routes::register_routes()
        .layer(axum::extract::Extension(db_service.clone()))
        .layer(cors)
        .layer(security_headers_layer())
        .layer(frame_options_layer())
        .layer(cache_control_layer())
        .layer(referrer_policy_layer())
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(max_body_size));

    let app = match (rate_limit_enabled, trust_proxy_headers) {
        (true, true) => {
            info!("Rate limiting enabled (trusting proxy headers)");
            app.layer(configure_rate_limiter_proxy())
        }
        (true, false) => {
            info!("Rate limiting enabled (using peer IP)");
            app.layer(configure_rate_limiter_peer())
        }
        (false, _) => {
            warn!("Rate limiting disabled");
            app
        }
    };

    let listener = TcpListener::bind(&bind_address).await.unwrap_or_else(|e| {
        error!("Failed to bind to address {}: {}", bind_address, e);
        std::process::exit(1);
    });

    info!("Server running on http://{}", bind_address);

    let health_check_db = db_service;
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
                        std::process::exit(1);
                    }
                }
            }
        }
    });

    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        error!("Server failed to start: {}", e);
        std::process::exit(1);
    }
}
