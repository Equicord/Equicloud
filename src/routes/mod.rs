use axum::Router;

pub mod health;
pub mod metrics;
pub mod v1;
pub mod v2;

pub fn register_routes() -> Router {
    Router::new()
        .merge(health::register())
        .merge(metrics::register())
        .merge(v1::register())
        .merge(v2::register())
}
