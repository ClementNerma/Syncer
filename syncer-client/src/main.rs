#![forbid(unsafe_code)]
#![forbid(unused_must_use)]
#![warn(unused_crate_dependencies)]

mod cmd;
mod diffing;
mod logging;

use std::{
    future::Future,
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use cmd::Args;
use colored::Colorize;
use dialoguer::Confirm;
use futures_util::TryStreamExt;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use reqwest::{header::AUTHORIZATION, Body, Client, Url};
use syncer_common::{
    snapshot::{
        make_snapshot, SnapshotFileMetadata, SnapshotItemMetadata, SnapshotOptions, SnapshotResult,
    },
    PING_ANSWER,
};
use time::OffsetDateTime;
use tokio::{
    fs::File,
    sync::{Mutex, RwLock},
    task::JoinSet,
    try_join,
};
use tokio_util::codec::{BytesCodec, Decoder};

use crate::{
    diffing::{
        build_diff, Diff, DiffItemAdded, DiffItemDeleted, DiffItemModified, DiffItemTypeChanged,
    },
    logging::PRINT_DEBUG_MESSAGES,
};

#[tokio::main]
async fn main() {
    if let Err(err) = inner_main().await {
        error!("{err:?}");
        std::process::exit(1);
    }
}

async fn inner_main() -> Result<()> {
    let Args {
        data_dir,
        address,
        server_secret,
        verbose,
        max_parallel_transfers,
        ignore_items,
        ignore_exts,
    } = Args::parse();

    if verbose {
        PRINT_DEBUG_MESSAGES.store(true, Ordering::SeqCst);
    }

    debug!("Started.");

    if !data_dir.is_dir() {
        bail!("Provided data directory was not found");
    }

    // ======================================================= //
    // =
    // = Ping the server
    // =
    // ======================================================= //

    debug!("Pinging server...");

    let url = Url::parse(&address).context("Failed to parse provided server address")?;

    let ping_text = Client::new()
        .get(url.clone())
        .header(AUTHORIZATION, format!("Bearer {server_secret}"))
        .send()
        .await
        .context("Failed to ping server")?
        .text()
        .await
        .context("Failed to convert response to plain text")?;

    if ping_text != PING_ANSWER {
        bail!("Server did not respond '{PING_ANSWER}' to ping request, but '{ping_text}'");
    }

    debug!("Server answered ping request correctly '{PING_ANSWER}'");

    // ======================================================= //
    // =
    // = Build local and remote snapshots
    // =
    // ======================================================= //

    info!("Building snapshots...");

    let options = SnapshotOptions {
        ignore_paths: ignore_items
            .iter()
            .filter(|item| Path::new(item).is_absolute())
            .map(|item| item.strip_prefix('/').unwrap().to_string())
            .collect(),

        ignore_names: ignore_items
            .into_iter()
            .filter(|item| !Path::new(item).is_absolute())
            .collect(),

        ignore_exts,
    };

    let multi_progress = MultiProgress::new();

    let local_pb = multi_progress.add(async_spinner());
    let remote_pb =
        multi_progress.add(async_spinner().with_message("Building snapshot on server..."));

    local_pb.enable_steady_tick(Duration::from_millis(150));
    remote_pb.enable_steady_tick(Duration::from_millis(150));

    let (local, remote) = try_join!(
        async_with_spinner(local_pb, |pb| make_snapshot(data_dir.clone(), pb, &options)),
        async_with_spinner(remote_pb, |_| build_remote_snapshot(
            &url,
            &server_secret,
            &options
        ))
    )?;

    for msg in local.debug {
        debug!("[snapshot:local] {msg}");
    }

    for msg in remote.debug {
        debug!("[snapshot:remote] {msg}");
    }

    // ======================================================= //
    // =
    // = Perform snapshots diffing and display
    // =
    // ======================================================= //

    info!("Diffing...");

    let Diff {
        added,
        modified,
        type_changed,
        deleted,
    } = build_diff(&local.snapshot, &remote.snapshot);

    let modified = modified
        .into_iter()
        .filter(|(path, DiffItemModified { prev, new })| {
            let SnapshotFileMetadata {
                size,
                last_modif_date,
                last_modif_date_ns: _,
            } = new;

            if *size != prev.size {
                return true;
            }

            let truncated_timestamp_diff = last_modif_date.abs_diff(prev.last_modif_date);

            if truncated_timestamp_diff <= 1 {
                debug!("Ignoring modified item '{path}' as modification time is no more than 2 seconds.");
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>();

    if added.is_empty() && modified.is_empty() && type_changed.is_empty() && deleted.is_empty() {
        success!("Nothing to do!");
        return Ok(());
    }

    if !added.is_empty() {
        info!("Added:");

        for (path, added) in &added {
            match added.new {
                SnapshotItemMetadata::Directory => {
                    println!(" {}", format!("{}/", path).bright_green())
                }
                SnapshotItemMetadata::File(m) => println!(
                    " {} {}",
                    path.bright_green(),
                    format!("({})", HumanBytes(m.size)).bright_yellow()
                ),
            }
        }

        println!();
    }

    if !modified.is_empty() {
        info!("Modified:");

        for (path, DiffItemModified { prev, new }) in &modified {
            let how = if prev.size != new.size {
                format!("({} => {})", HumanBytes(prev.size), HumanBytes(new.size))
            } else if prev.last_modif_date != new.last_modif_date
                || prev.last_modif_date_ns != new.last_modif_date_ns
            {
                let prev =
                    OffsetDateTime::from_unix_timestamp(prev.last_modif_date.try_into().unwrap())
                        .unwrap()
                        + Duration::from_nanos(prev.last_modif_date_ns.into());

                let new =
                    OffsetDateTime::from_unix_timestamp(new.last_modif_date.try_into().unwrap())
                        .unwrap()
                        + Duration::from_nanos(new.last_modif_date_ns.into());

                format!("({prev} => {new})")
            } else {
                unreachable!();
            };

            println!("{} {}", path.bright_yellow(), how.bright_yellow());
        }

        println!();
    }

    if !type_changed.is_empty() {
        info!("Type changed:");

        let type_letter = |m: SnapshotItemMetadata| match m {
            SnapshotItemMetadata::Directory => "D",
            SnapshotItemMetadata::File(_) => "F",
        };

        for (path, type_changed) in &type_changed {
            let message = format!(
                " {}{} ({} => {})",
                path,
                if matches!(type_changed.new, SnapshotItemMetadata::Directory) {
                    "/"
                } else {
                    ""
                },
                type_letter(type_changed.prev),
                type_letter(type_changed.new)
            );

            println!("{}", message.bright_yellow());
        }

        println!();
    }

    if !deleted.is_empty() {
        info!("Deleted:");

        for (path, deleted) in &deleted {
            match deleted.prev {
                SnapshotItemMetadata::Directory => {
                    info!(" {}", format!("{path}/").bright_red())
                }
                SnapshotItemMetadata::File(m) => info!(
                    " {} {}",
                    path.bright_red(),
                    format!("({})", HumanBytes(m.size)).bright_yellow()
                ),
            }
        }

        info!("");
    }

    // ======================================================= //
    // =
    // = Categorize operations to perform
    // =
    // ======================================================= //

    let to_create_dirs =
        added
            .iter()
            .filter_map(|(path, DiffItemAdded { new })| match new {
                SnapshotItemMetadata::Directory => Some(path),
                SnapshotItemMetadata::File(_) => None,
            })
            .chain(type_changed.iter().filter_map(
                |(path, DiffItemTypeChanged { prev: _, new })| match new {
                    SnapshotItemMetadata::Directory => Some(path),
                    SnapshotItemMetadata::File(_) => None,
                },
            ))
            .collect::<Vec<_>>();

    let to_transfer =
        added
            .iter()
            .filter_map(|(path, DiffItemAdded { new })| match new {
                SnapshotItemMetadata::Directory => None,
                SnapshotItemMetadata::File(mt) => Some((path.clone(), mt)),
            })
            .chain(
                modified
                    .iter()
                    .map(|(path, DiffItemModified { prev: _, new })| (path.clone(), new)),
            )
            .chain(type_changed.iter().filter_map(
                |(path, DiffItemTypeChanged { prev: _, new })| match new {
                    SnapshotItemMetadata::Directory => None,
                    SnapshotItemMetadata::File(mt) => Some((path.clone(), mt)),
                },
            ))
            .collect::<Vec<_>>();

    let to_delete = deleted
        .iter()
        .map(|(path, DiffItemDeleted { prev })| (path, prev))
        .chain(
            type_changed
                .iter()
                .map(|(path, DiffItemTypeChanged { prev, new: _ })| (path, prev)),
        )
        .rev()
        .collect::<Vec<_>>();

    let transfer_size = to_transfer.iter().map(|(_, mt)| mt.size).sum();

    info!(
        "Found a total of {} files (+ {} dirs) to transfer and {} to delete for a total of {}.",
        to_transfer.len().to_string().bright_green(),
        to_create_dirs.len().to_string().bright_green(),
        to_delete.len().to_string().bright_red(),
        format!("{}", HumanBytes(transfer_size)).bright_yellow()
    );

    let confirm = Confirm::new()
        .with_prompt("Continue?".bright_blue().to_string())
        .interact()?;

    if !confirm {
        warn!("Transfer was cancelled.");
        std::process::exit(1);
    }

    // ======================================================= //
    // =
    // = Prepare the progress bars for transfer
    // =
    // ======================================================= //

    let mp = MultiProgress::new();

    let pb_msg = Arc::new(RwLock::new(
        mp.add(
            ProgressBar::new(1)
                .with_style(ProgressStyle::with_template("{msg}").unwrap())
                .with_message("Running..."),
        ),
    ));

    let delete_pb = Arc::new(RwLock::new(
        mp.add(
            ProgressBar::new(to_delete.len() as u64).with_style(
                ProgressStyle::with_template(
                    "Deleting     : [{elapsed_precise}] {prefix} {bar:40} {pos}/{len} items",
                )
                .unwrap(),
            ),
        ),
    ));

    let create_dirs_pb = Arc::new(RwLock::new(
        mp.add(
            ProgressBar::new(to_create_dirs.len() as u64).with_style(
                ProgressStyle::with_template(
                    "Creating     : [{elapsed_precise}] {prefix} {bar:40} {pos}/{len} directories",
                )
                .unwrap(),
            ),
        ),
    ));

    let transfer_pb = Arc::new(RwLock::new(
        mp.add(
            ProgressBar::new(to_transfer.len() as u64).with_style(
                ProgressStyle::with_template(
                    "Transferring : [{elapsed_precise}] {prefix} {bar:40} {pos}/{len} files",
                )
                .unwrap(),
            ),
        ),
    ));

    let transfer_size_pb = Arc::new(RwLock::new( mp.add(
        ProgressBar::new(transfer_size ).with_style(
            ProgressStyle::with_template(
                "Transfer size: [{elapsed_precise}] {prefix} {bar:40} {bytes}/{total_bytes} ({binary_bytes_per_sec})",
            )
            .unwrap(),
        ),
    )));

    let errors = Arc::new(Mutex::new(vec![]));

    macro_rules! report_err {
        ($err: expr, $errors: expr, $pb_msg: expr) => {{
            let mut errors = $errors.lock().await;

            errors.push($err);
            $pb_msg.read().await.set_message(
                format!(
                    "Running... (encountered {} error(s))\n{}",
                    errors.len(),
                    errors
                        .iter()
                        .map(|err| format!("\n* {err}"))
                        .collect::<String>()
                )
                .bright_red()
                .to_string(),
            );
        }}
    }

    // ======================================================= //
    // =
    // = Delete items
    // =
    // ======================================================= //

    let mut task_pool = JoinSet::new();

    for (path, item) in to_delete {
        let path = String::clone(path);

        let errors = Arc::clone(&errors);
        let pb_msg = Arc::clone(&pb_msg);
        let delete_pb = Arc::clone(&delete_pb);

        let uri_item_type = match item {
            SnapshotItemMetadata::Directory => "dir",
            SnapshotItemMetadata::File(_) => "file",
        };

        let req = Client::new()
            .delete(url.join(&format!("/fs/{uri_item_type}/delete"))?)
            .header(AUTHORIZATION, format!("Bearer {server_secret}"))
            .query(&[("path", &path)]);

        task_pool.spawn(async move {
            match req.send().await {
                Ok(data) => {
                    if let Err(err) = data.error_for_status() {
                        report_err!(
                            format!("Failed to delete item at '{path}': {err}"),
                            errors,
                            pb_msg
                        );
                    }
                }

                Err(err) => {
                    report_err!(
                        format!("Failed to send deletion request for item '{path}': {err}"),
                        errors,
                        pb_msg
                    );
                }
            }

            delete_pb.read().await.inc(1);
        });
    }

    while let Some(result) = task_pool.join_next().await {
        result?;
    }

    // ======================================================= //
    // =
    // = Create directories
    // =
    // ======================================================= //

    let mut task_pool = JoinSet::new();

    for path in &to_create_dirs {
        let path = String::clone(path);

        let errors = Arc::clone(&errors);
        let pb_msg = Arc::clone(&pb_msg);
        let create_dirs_pb = Arc::clone(&create_dirs_pb);

        let req = Client::new()
            .post(url.join("/fs/dir/create")?)
            .header(AUTHORIZATION, format!("Bearer {server_secret}"))
            .query(&[("path", &path)]);

        task_pool.spawn(async move {
            match req.send().await {
                Ok(data) => {
                    if let Err(err) = data.error_for_status() {
                        report_err!(
                            format!("Failed to create directory '{path}': {err}"),
                            errors,
                            pb_msg
                        );
                    }
                }

                Err(err) => {
                    report_err!(
                        format!("Failed to send directory creation request '{path}': {err}"),
                        errors,
                        pb_msg
                    );
                }
            }

            create_dirs_pb.read().await.inc(1);
        });
    }

    while let Some(result) = task_pool.join_next().await {
        result?;
    }

    // ======================================================= //
    // =
    // = Transfer files
    // =
    // ======================================================= //

    let mut task_pool = JoinSet::new();

    for (
        path,
        SnapshotFileMetadata {
            last_modif_date,
            last_modif_date_ns,
            size,
        },
    ) in to_transfer
    {
        while task_pool.len() > max_parallel_transfers {
            task_pool.join_next().await.unwrap()?;
        }

        let data_dir = data_dir.clone();

        let errors = Arc::clone(&errors);
        let pb_msg = Arc::clone(&pb_msg);
        let transfer_size_pb = Arc::clone(&transfer_size_pb);

        transfer_pb.read().await.inc(1);

        match File::open(data_dir.join(&path)).await {
            Err(err) => {
                report_err!(
                    format!("Failed to open file '{path}' for transfer: {err}"),
                    errors,
                    pb_msg
                );
            }

            Ok(file) => {
                let transfer_size_pb = transfer_size_pb.clone();

                let stream = BytesCodec::new().framed(file).inspect_ok(move |chunk| {
                    let size = chunk.len() as u64;
                    let transfer_size_pb = Arc::clone(&transfer_size_pb);

                    tokio::spawn(async move {
                        transfer_size_pb.read().await.inc(size);
                    });
                });

                let file_body = Body::wrap_stream(stream);

                let req = Client::new()
                    .post(url.join("/fs/file/write")?)
                    .query(&[("path", &path)])
                    .query(&[("last_modification", last_modif_date)])
                    .query(&[("last_modification_ns", last_modif_date_ns)])
                    .query(&[("size", size)])
                    .header(AUTHORIZATION, format!("Bearer {server_secret}"))
                    .body(file_body);

                task_pool.spawn(async move {
                    match req.send().await {
                        Ok(data) => {
                            if let Err(err) = data.error_for_status() {
                                report_err!(
                                    format!("Failed to transfer file '{path}': {err}"),
                                    errors,
                                    pb_msg
                                );
                            }
                        }

                        Err(err) => {
                            report_err!(
                                format!("Failed to complete request for file '{path}': {err}"),
                                errors,
                                pb_msg
                            );
                        }
                    }
                });
            }
        }
    }

    while let Some(result) = task_pool.join_next().await {
        result?;
    }

    // ======================================================= //
    // =
    // = Done!
    // =
    // ======================================================= //

    let errors = errors.lock().await;

    if !errors.is_empty() {
        bail!("{} error(s) occurred.", errors.len(),);
    }

    success!("Synchronized successfully.");
    Ok(())
}

async fn build_remote_snapshot(
    url: &Url,
    server_secret: &str,
    options: &SnapshotOptions,
) -> Result<SnapshotResult> {
    let res = Client::new()
        .post(url.join("snapshot")?)
        .header(AUTHORIZATION, format!("Bearer {server_secret}"))
        .json(options)
        .send()
        .await
        .context("Failed to get remote snapshot")?;

    let res = res
        .error_for_status()
        .context("Failed to build remote snapshot")?;

    let res = res
        .json()
        .await
        .context("Failed to deserialize server's response")?;

    Ok(res)
}

fn async_spinner() -> ProgressBar {
    ProgressBar::new_spinner()
        .with_style(ProgressStyle::with_template("{spinner} [{elapsed_precise}] {msg}").unwrap())
}

async fn async_with_spinner<F: Future<Output = Result<T, E>>, T, E>(
    pb: ProgressBar,
    task: impl FnOnce(Box<dyn Fn(String) + Send + Sync>) -> F,
) -> Result<T, E> {
    let pb_closure = pb.clone();

    let result = task(Box::new(move |msg| pb_closure.set_message(msg))).await;

    pb.set_style(pb.style().tick_chars(&format!(
        " {}",
        match result {
            Ok(_) => '✅',
            Err(_) => '❌',
        }
    )));

    pb.finish();

    result
}
