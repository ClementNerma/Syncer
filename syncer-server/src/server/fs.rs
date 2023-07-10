use std::path::{Path, PathBuf};

use anyhow::Context;
use axum::{
    body::StreamBody,
    extract::{BodyStream, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use filetime::FileTime;
use futures_util::StreamExt;
use serde::Deserialize;
use syncer_common::snapshot::{make_snapshot, SnapshotOptions, SnapshotResult};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
};
use tokio_util::io::ReaderStream;
use tracing::trace;

use crate::{handle_err, server_err, throw_err};

use super::{ServerResult, SharedState};

#[derive(Deserialize)]
pub struct PathQuery {
    path: PathBuf,
}

fn _validate_data_dir_descendent(path: &Path, data_dir: &Path) -> ServerResult<PathBuf> {
    let path = data_dir.join(path);

    match path.strip_prefix(data_dir) {
        Ok(_) => Ok(path),
        Err(_) => Err(server_err!(
            BAD_REQUEST,
            "Provided path is not a descendent of the data directory"
        )),
    }
}

pub async fn snapshot(
    state: State<SharedState>,
    Json(options): Json<SnapshotOptions>,
) -> ServerResult<Json<SnapshotResult>> {
    match make_snapshot(
        state.read().await.data_dir.clone(),
        |msg| trace!("{msg}"),
        &options,
    )
    .await
    {
        Ok(snapshot) => Ok(Json(snapshot)),
        Err(err) => throw_err!(INTERNAL_SERVER_ERROR, format!("{err}")),
    }
}

pub async fn read_file(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
) -> ServerResult<impl IntoResponse> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

    if !path.exists() {
        throw_err!(BAD_REQUEST, "Provided path does not exist");
    }

    if path.is_symlink() {
        throw_err!(BAD_REQUEST, "Provided path is a symbolic link");
    }

    if path.is_dir() {
        throw_err!(BAD_REQUEST, "Provided path is a directory");
    }

    let file = File::open(&path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    let stream = ReaderStream::new(file);
    let body = StreamBody::new(stream);

    Ok(body)
}

#[derive(Deserialize)]
pub struct FileMetadataQuery {
    last_modification: i64,
    last_modification_ns: u32,
    size: u64,
}

pub async fn write_file(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
    Query(FileMetadataQuery {
        last_modification,
        last_modification_ns,
        size,
    }): Query<FileMetadataQuery>,
    mut stream: BodyStream,
) -> ServerResult<()> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

    if path.is_symlink() {
        throw_err!(BAD_REQUEST, "Provided path is a symbolic link");
    }

    if path.is_dir() {
        throw_err!(BAD_REQUEST, "Provided path is a directory");
    }

    let tmp_path = path.with_file_name(format!(
        ".tmp-{last_modification}-{last_modification_ns}-{size}"
    ));

    let mut tmp_file = File::create(&tmp_path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    let mut written = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(handle_err!(INTERNAL_SERVER_ERROR))?;
        written += chunk.len();

        tmp_file
            .write_all(&chunk)
            .await
            .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;
    }

    if u64::try_from(written).unwrap() != size {
        throw_err!(
            BAD_REQUEST,
            "Provided size does not match transmitted content"
        );
    }

    fs::rename(&tmp_path, &path)
        .await
        .context("Failed to rename temporary file to final name")
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    tokio::task::spawn_blocking(move || {
        filetime::set_file_mtime(
            path,
            FileTime::from_unix_time(last_modification, last_modification_ns),
        )
        .context("Failed to set modification time")
    })
    .await
    .context("Failed to run modification time setter")
    .map_err(handle_err!(INTERNAL_SERVER_ERROR))?
    .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    Ok(())
}

pub async fn remove_file(
    state: State<SharedState>,
    Query(PathQuery { path }): Query<PathQuery>,
) -> ServerResult<()> {
    let path = _validate_data_dir_descendent(&path, &state.read().await.data_dir)?;

    if !path.exists() {
        throw_err!(BAD_REQUEST, "Provided path does not exist");
    }

    if path.is_symlink() {
        throw_err!(BAD_REQUEST, "Provided path is a symbolic link");
    }

    if !path.is_file() {
        throw_err!(BAD_REQUEST, "Provided path is not a file");
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
        throw_err!(CONFLICT, "Provided path already exists");
    }

    if path.is_symlink() {
        throw_err!(CONFLICT, "Provided path is a symbolic link");
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
        throw_err!(CONFLICT, "Provided path does not exist");
    }

    if path.is_symlink() {
        throw_err!(CONFLICT, "Provided path is a symbolic link");
    }

    if !path.is_dir() {
        throw_err!(CONFLICT, "Provided path is not a directory");
    }

    fs::remove_dir(&path)
        .await
        .map_err(handle_err!(INTERNAL_SERVER_ERROR))?;

    Ok(())
}
