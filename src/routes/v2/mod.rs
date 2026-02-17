use axum::{
    Router, middleware,
    routing::{get, post},
};

pub mod data;
pub mod manifest;
pub mod sync;

pub fn register() -> Router {
    Router::new()
        .route("/v2/manifest", get(manifest::get_manifest))
        .route(
            "/v2/data/{*key}",
            get(data::get_data)
                .put(data::put_data)
                .delete(data::delete_data),
        )
        .route("/v2/sync", post(sync::delta_sync))
        .route_layer(middleware::from_fn(
            crate::middleware::auth::auth_middleware,
        ))
}
