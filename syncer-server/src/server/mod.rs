mod fs;

use crate::server::fs::{create_dir, remove_dir, remove_file};

use self::fs::{read_file, snapshot, write_file};

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{bail, Result};
use axum::{
    extract::State,
    headers::{authorization::Bearer, Authorization},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
    Router, Server, TypedHeader,
};
use syncer_common::PING_ANSWER;
use tokio::sync::RwLock;
use tower_http::{
    cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::info;

pub struct StateInner {
    pub data_dir: PathBuf,
    pub secret: String,
}

type SharedState = Arc<RwLock<StateInner>>;

pub async fn serve(addr: &SocketAddr, secret: String, data_dir: PathBuf) -> Result<()> {
    let cors = CorsLayer::new()
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any())
        .allow_origin(AllowOrigin::any());

    let state = StateInner { data_dir, secret };

    let state = Arc::new(RwLock::new(state));

    // TODO: implement compression and decompression
    let app = Router::new()
        .route("/", get(ping))
        .route("/snapshot", post(snapshot))
        .route("/fs/file/read", get(read_file))
        // TODO: use this ^^^^^
        .route("/fs/file/write", post(write_file))
        .route("/fs/file/delete", delete(remove_file))
        .route("/fs/dir/create", post(create_dir))
        .route("/fs/dir/delete", delete(remove_dir))
        .with_state(Arc::clone(&state))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(Arc::clone(&state), auth))
        .layer(cors);

    info!("Starting server at {addr}...");

    Server::bind(addr).serve(app.into_make_service()).await?;

    bail!("Server exited.");
}

#[macro_export]
macro_rules! server_err {
    ($variant: ident, $msg: expr) => {{
        ::tracing::error!("> Request failed: {}", $msg);
        (StatusCode::$variant, $msg).into_response()
    }};
}

#[macro_export]
macro_rules! handle_err {
    ($variant: ident) => {
        |err| $crate::server_err!($variant, format!("{err}"))
    };
}

#[macro_export]
macro_rules! throw_err {
    ($variant: ident, $err: expr) => {
        return Err($crate::server_err!($variant, $err))
    };
}

type ServerResult<T> = Result<T, Response>;

async fn auth<B>(
    state: State<SharedState>,
    TypedHeader(auth_header): TypedHeader<Authorization<Bearer>>,
    req: Request<B>,
    next: Next<B>,
) -> Result<Response, (StatusCode, &'static str)> {
    if state.read().await.secret == auth_header.token() {
        Ok(next.run(req).await)
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "Invalid secret token provided in Authorization header",
        ))
    }
}

async fn ping() -> &'static str {
    PING_ANSWER
}
