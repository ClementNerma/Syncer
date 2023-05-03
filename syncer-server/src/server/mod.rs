mod fs;

use crate::server::fs::{create_dir, remove_dir, remove_file};

use self::fs::{read_file, snapshot, write_file};

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{bail, Result};
use axum::{
    http::StatusCode,
    routing::{delete, get, put},
    Router, Server,
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
}

type SharedState = Arc<RwLock<StateInner>>;

pub async fn serve(addr: &SocketAddr, data_dir: PathBuf) -> Result<()> {
    let cors = CorsLayer::new()
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any())
        .allow_origin(AllowOrigin::any());

    let state = StateInner { data_dir };

    // TODO: implement compression and decompression
    let app = Router::new()
        .route("/", get(ping))
        .route("/snapshot", get(snapshot))
        .route("/fs/file/read", get(read_file))
        .route("/fs/file/write", put(write_file))
        .route("/fs/file/delete", delete(remove_file))
        .route("/fs/dir/create", put(create_dir))
        .route("/fs/dir/delete", delete(remove_dir))
        .with_state(Arc::new(RwLock::new(state)))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    info!("Starting server at {addr}...");

    Server::bind(addr).serve(app.into_make_service()).await?;

    bail!("Server exited.");
}

#[macro_export]
macro_rules! handle_err {
    ($variant: ident) => {
        |err| (StatusCode::$variant, format!("{err}"))
    };
}

type ServerError = (StatusCode, String);
type ServerResult<T> = Result<T, ServerError>;

async fn ping() -> &'static str {
    PING_ANSWER
}
