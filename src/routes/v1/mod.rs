use axum::{
    Router, middleware,
    routing::{delete, get, head},
};

pub mod delete;
pub mod oauth;
pub mod settings;

pub fn register() -> Router {
    let public_routes = Router::new()
        .route("/v1", get(delete::v1_status_pong))
        .route("/v1/", get(delete::v1_status_pong))
        .route("/v1/oauth/callback", get(oauth::callback::oauth_callback))
        .route("/v1/oauth/settings", get(oauth::settings::oauth_settings));

    let auth_routes = Router::new()
        .route(
            "/v1/settings",
            head(settings::head_settings)
                .get(settings::get_settings)
                .put(settings::put_settings)
                .delete(settings::delete_settings),
        )
        .route("/v1", delete(delete::delete_all_user_data))
        .route("/v1/", delete(delete::delete_all_user_data))
        .route_layer(middleware::from_fn(
            crate::middleware::auth::auth_middleware,
        ));

    public_routes.merge(auth_routes)
}
