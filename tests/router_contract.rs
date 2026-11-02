//! Router contract integration tests.
//!
//! These exercise the full HTTP router (form/query normalization, `.view`
//! routes, GET+POST, authentication, and XML/JSON error rendering) without a
//! running server, using a temporary `SQLite` database.

use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use suboxide::app::{AppState, CorsConfig, create_router};
use suboxide::crypto::password::hash_password;
use suboxide::db::{DbConfig, DbPool, NewUser, UserRepository, initialize_database};
use suboxide::lastfm::LastFmClient;
use tower::ServiceExt;

static DB_SEQ: AtomicU32 = AtomicU32::new(0);

fn temp_db_path() -> String {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "suboxide-router-test-{}-{seq}.db",
        std::process::id()
    ));
    let path = path.to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&path);
    path
}

/// Build a initialized database pool with an admin user `t` (password `t`).
fn test_pool() -> DbPool {
    let config = DbConfig::new(temp_db_path());
    let pool = config.build_pool().expect("pool must build");
    let mut conn = pool.get().expect("connection must be available");
    initialize_database(&mut conn).expect("schema must initialize");

    // Admin user with a known subsonic password.
    let password_hash = hash_password("t").expect("password must hash");
    UserRepository::new(pool.clone())
        .create(&NewUser::admin("t", &password_hash, "t"))
        .expect("admin user must be created");

    pool
}

/// Build a router backed by the shared test database.
fn test_app() -> Router {
    let state = AppState::new(
        test_pool(),
        LastFmClient::new(String::new(), String::new()).expect("disabled"),
    );
    create_router(state, &CorsConfig::default())
}

fn get(uri: &str) -> Request {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("request must build")
}

fn post_form(uri: &str, body: &str) -> Request {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .expect("request must build")
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body must read");
    String::from_utf8(bytes.to_vec()).expect("body must be UTF-8")
}

#[tokio::test]
async fn ping_works_on_base_and_view_routes() {
    let app = test_app();
    let base = app
        .clone()
        .oneshot(get("/rest/ping?u=t&p=t&v=1.16.1&c=t&f=json"))
        .await
        .expect("router must respond");
    assert_eq!(base.status(), StatusCode::OK);

    let view = app
        .oneshot(get("/rest/ping.view?u=t&p=t&v=1.16.1&c=t&f=json"))
        .await
        .expect("router must respond");
    assert_eq!(view.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_form_authenticates_and_returns_json() {
    let app = test_app();
    let response = app
        .oneshot(post_form("/rest/ping?u=t&v=1.16.1&c=t&f=json", "p=t"))
        .await
        .expect("router must respond");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(
        body.contains("\"status\":\"ok\""),
        "unexpected body: {body}"
    );
    assert!(
        body.contains("\"version\":\"1.16.1\""),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn missing_credentials_return_json_error() {
    let app = test_app();
    let response = app
        .oneshot(get("/rest/ping?u=t&v=1.16.1&c=t&f=json"))
        .await
        .expect("router must respond");
    assert_eq!(response.status(), StatusCode::OK); // Subsonic errors are 200
    let body = body_string(response).await;
    assert!(
        body.contains("\"status\":\"failed\""),
        "unexpected body: {body}"
    );
    assert!(body.contains("\"code\":10"), "unexpected body: {body}");
}

#[tokio::test]
async fn wrong_password_returns_credentials_error_in_json() {
    let app = test_app();
    let response = app
        .oneshot(get("/rest/ping?u=t&p=wrong&v=1.16.1&c=t&f=json"))
        .await
        .expect("router must respond");
    let body = body_string(response).await;
    assert!(body.contains("\"code\":40"), "unexpected body: {body}");
}

#[tokio::test]
async fn xml_error_renders_subsonic_envelope() {
    let app = test_app();
    let response = app
        .oneshot(get("/rest/ping?u=t&p=wrong&v=1.16.1&c=t"))
        .await
        .expect("router must respond");
    let body = body_string(response).await;
    assert!(
        body.contains("<subsonic-response"),
        "unexpected body: {body}"
    );
    assert!(
        body.contains("status=\"failed\""),
        "unexpected body: {body}"
    );
    assert!(body.contains("code=\"40\""), "unexpected body: {body}");
}

#[tokio::test]
async fn unsupported_format_returns_xml_error() {
    let app = test_app();
    let response = app
        .oneshot(get("/rest/ping?u=t&p=t&v=1.16.1&c=t&f=bogus"))
        .await
        .expect("router must respond");
    let body = body_string(response).await;
    assert!(
        body.contains("<subsonic-response"),
        "unexpected body: {body}"
    );
    assert!(
        body.contains("status=\"failed\""),
        "unexpected body: {body}"
    );
}
