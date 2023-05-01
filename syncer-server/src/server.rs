use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{bail, Result};
use axum::{
    body::StreamBody,
    extract::{BodyStream, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router, Server,
};
use futures_util::StreamExt;
use syncer_common::{
    snapshot::{make_snapshot, Snapshot},
    PING_ANSWER,
};
use tokio::{fs::File, io::AsyncWriteExt, sync::RwLock};
use tokio_util::io::ReaderStream;
use tower_http::{
    cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::info;

struct StateInner {
    data_dir: PathBuf,
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
        .route("/file/:path", get(read_file).put(write_file))
        .with_state(Arc::new(RwLock::new(state)))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    info!("Starting server at {addr}...");

    Server::bind(addr).serve(app.into_make_service()).await?;

    bail!("Server exited.");
}

macro_rules! handle_err {
    ($variant: ident) => {
        |err| (StatusCode::$variant, format!("{err}"))
    };
}

async fn ping() -> &'static str {
    PING_ANSWER
}

async fn snapshot(state: State<SharedState>) -> Result<Json<Snapshot>, (StatusCode, String)> {
    match make_snapshot(state.read().await.data_dir.clone(), |msg| info!("{msg}")).await {
        Ok(snapshot) => Ok(Json(snapshot)),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"))),
    }
}

// TODO: have a "last_modified" parameter to ensure file didn't change
// between snapshot and calling this network function
async fn read_file(
    state: State<SharedState>,
    Path(path): Path<PathBuf>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let data_dir = &state.read().await.data_dir;

    let path = data_dir.join(&path);

    if path.strip_prefix(data_dir).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is not a descendent of the data directory".to_string(),
        ));
    }

    if !path.exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path does not exist".to_string(),
        ));
    }

    if path.is_symlink() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is a symbolic link".to_string(),
        ));
    }

    if path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is a directory".to_string(),
        ));
    }

    let file = File::open(&path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    let stream = ReaderStream::new(file);
    let body = StreamBody::new(stream);

    Ok(body)
}

async fn write_file(
    state: State<SharedState>,
    Path(path): Path<PathBuf>,
    mut stream: BodyStream,
) -> Result<(), (StatusCode, String)> {
    let data_dir = &state.read().await.data_dir;

    let path = data_dir.join(&path);

    if path.strip_prefix(data_dir).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is not a descendent of the data directory".to_string(),
        ));
    }

    if path.is_symlink() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is a symbolic link".to_string(),
        ));
    }

    if path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is a directory".to_string(),
        ));
    }

    let mut file = File::create(&path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(handle_err!(BAD_REQUEST))?;

        file.write_all(&chunk)
            .await
            .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;
    }

    Ok(())
}
