use std::path::{Path, PathBuf};

use axum::{
    body::StreamBody,
    extract::{BodyStream, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use futures_util::StreamExt;
use serde::Deserialize;
use syncer_common::snapshot::{make_snapshot, Snapshot};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
};
use tokio_util::io::ReaderStream;
use tracing::info;

use crate::handle_err;

use super::{ServerResult, SharedState};

#[derive(Deserialize)]
pub struct PathQuery {
    path: PathBuf,
}

fn _validate_data_dir_descendent(path: &Path, data_dir: &Path) -> ServerResult<PathBuf> {
    let path = data_dir.join(path);

    path.strip_prefix(data_dir)
        .map(Path::to_path_buf)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Provided path is not a descendent of the data directory".to_string(),
            )
        })
}

pub async fn snapshot(state: State<SharedState>) -> ServerResult<Json<Snapshot>> {
    match make_snapshot(state.read().await.data_dir.clone(), |msg| info!("{msg}")).await {
        Ok(snapshot) => Ok(Json(snapshot)),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"))),
    }
}

pub async fn read_file(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
) -> ServerResult<impl IntoResponse> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

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

pub async fn write_file(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
    mut stream: BodyStream,
) -> ServerResult<()> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

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

pub async fn remove_file(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
) -> ServerResult<()> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

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

    if !path.is_file() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is not a file".to_string(),
        ));
    }

    fs::remove_file(&path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    Ok(())
}

pub async fn create_dir(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
) -> ServerResult<()> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

    if path.exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path already exists".to_string(),
        ));
    }

    if path.is_symlink() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is a symbolic link".to_string(),
        ));
    }

    fs::create_dir(&path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    Ok(())
}

pub async fn remove_dir(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
) -> ServerResult<()> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

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

    if !path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provided path is not a directory".to_string(),
        ));
    }

    fs::remove_dir(&path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    Ok(())
}
