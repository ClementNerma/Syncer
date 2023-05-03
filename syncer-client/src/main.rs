#![forbid(unsafe_code)]
#![forbid(unused_must_use)]
#![warn(unused_crate_dependencies)]

mod cmd;
mod diffing;
mod logging;

use std::{future::Future, sync::atomic::Ordering, time::Duration};

use anyhow::{bail, Context, Result};
use clap::Parser;
use cmd::Args;
use colored::Colorize;
use dialoguer::Confirm;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use reqwest::{Body, Client, Url};
use syncer_common::{
    snapshot::{make_snapshot, Snapshot, SnapshotFileMetadata, SnapshotItemMetadata},
    PING_ANSWER,
};
use tokio::{fs::File, join};
use tokio_util::codec::{BytesCodec, FramedRead};

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
        verbose,
    } = Args::parse();

    if verbose {
        PRINT_DEBUG_MESSAGES.store(true, Ordering::SeqCst);
    }

    debug!("Started.");

    if !data_dir.is_dir() {
        bail!("Provided data directory was not found");
    }

    debug!("Pinging server...");

    let url = Url::parse(&address).context("Failed to parse provided server address")?;

    let ping_text = reqwest::get(url.clone())
        .await
        .context("Failed to ping server")?
        .text()
        .await
        .context("Failed to convert response to plain text")?;

    if ping_text != PING_ANSWER {
        bail!("Server did not respond '{PING_ANSWER}' to ping request, but '{ping_text}'");
    }

    debug!("Server answered ping request correctly '{PING_ANSWER}'");

    info!("Building snapshots...");

    let multi_progress = MultiProgress::new();

    let local_pb = multi_progress.add(async_spinner());
    let remote_pb =
        multi_progress.add(async_spinner().with_message("Building snapshot on server..."));

    local_pb.enable_steady_tick(Duration::from_millis(150));
    remote_pb.enable_steady_tick(Duration::from_millis(150));

    let (local, remote) = join!(
        async_with_spinner(local_pb, |pb| make_snapshot(data_dir.clone(), pb)),
        async_with_spinner(remote_pb, |_| build_remote_snapshot(&url))
    );

    let local = local.context("Failed to build local snapshot")?;
    let remote = remote.context("Failed to build remote snapshot")?;

    info!("Diffing...");

    let Diff {
        added,
        modified,
        type_changed,
        deleted,
    } = build_diff(&local, &remote);

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

        for (path, modified) in &modified {
            println!(
                "{}",
                format!(" {} ({})", path, HumanBytes(modified.new.size)).bright_yellow()
            );
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
                SnapshotItemMetadata::File(mt) => Some((path, mt)),
            })
            .chain(
                modified
                    .iter()
                    .map(|(path, DiffItemModified { prev: _, new })| (path, new)),
            )
            .chain(type_changed.iter().filter_map(
                |(path, DiffItemTypeChanged { prev: _, new })| match new {
                    SnapshotItemMetadata::Directory => None,
                    SnapshotItemMetadata::File(mt) => Some((path, mt)),
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

    let mp = MultiProgress::new();

    let pb_msg = mp.add(
        ProgressBar::new(1 as u64)
            .with_style(ProgressStyle::with_template("{msg}").unwrap())
            .with_message("Running..."),
    );

    let pb_style =
        ProgressStyle::with_template("[{elapsed_precise}] {prefix} {bar:40} {pos}/{len} {msg}")
            .unwrap();

    let delete_pb = mp.add(
        ProgressBar::new(to_delete.len() as u64)
            .with_style(pb_style.clone())
            .with_prefix("Deleting     :"),
    );

    let create_dirs_pb = mp.add(
        ProgressBar::new(to_create_dirs.len() as u64)
            .with_style(pb_style.clone())
            .with_prefix("Creating dirs:"),
    );

    let transfer_pb = mp.add(
        ProgressBar::new(to_transfer.len() as u64)
            .with_style(pb_style.clone())
            .with_prefix("Transferring :"),
    );

    let transfer_size_pb = mp.add(
        ProgressBar::new(transfer_size as u64)
            .with_style(pb_style.clone())
            .with_prefix("Transfer size:"),
    );

    let mut errors = vec![];

    let mut report_err = |err: String| {
        errors.push(err);
        pb_msg.set_message(
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
    };

    for (path, item) in &to_delete {
        let uri_item_type = match item {
            SnapshotItemMetadata::Directory => "dir",
            SnapshotItemMetadata::File(_) => "file",
        };

        let req = Client::new()
            .delete(url.join(&format!("/fs/{uri_item_type}/delete"))?)
            .query(&[("path", path)]);

        match req.send().await {
            Ok(data) => {
                if let Err(err) = data.error_for_status() {
                    report_err(format!("Failed to delete item at '{path}': {err}"));
                }
            }

            Err(err) => {
                report_err(format!(
                    "Failed to send deletion request for item '{path}': {err}"
                ));
            }
        }

        delete_pb.inc(1);
    }

    for path in &to_create_dirs {
        let req = Client::new()
            .put(url.join(&format!("/fs/dir/create"))?)
            .query(&[("path", path)]);

        match req.send().await {
            Ok(data) => {
                if let Err(err) = data.error_for_status() {
                    report_err(format!("Failed to create directory '{path}': {err}"));
                }
            }

            Err(err) => {
                report_err(format!(
                    "Failed to send directory creation request '{path}': {err}"
                ));
            }
        }

        create_dirs_pb.inc(1);
    }

    for (
        path,
        SnapshotFileMetadata {
            last_modif_date,
            last_modif_date_ns,
            size,
        },
    ) in &to_transfer
    {
        match File::open(data_dir.join(path)).await {
            Err(err) => report_err(format!("Failed to open file '{path}' for transfer: {err}")),

            Ok(file) => {
                let stream = FramedRead::new(file, BytesCodec::new());
                let file_body = Body::wrap_stream(stream);

                let req = Client::new()
                    .put(url.join("/fs/file/write")?)
                    .query(&[("path", path)])
                    .query(&[("last_modif_date", last_modif_date)])
                    .query(&[("last_modif_date_ns", last_modif_date_ns)])
                    .query(&[("size", size)])
                    .body(file_body);

                match req.send().await {
                    Ok(data) => {
                        if let Err(err) = data.error_for_status() {
                            report_err(format!("Failed to transfer file '{path}': {err}"));
                        }
                    }

                    Err(err) => {
                        report_err(format!("Failed to transfer file '{path}': {err}"));
                    }
                }
            }
        }

        transfer_pb.inc(1);
        transfer_size_pb.inc(*size);
    }

    if !errors.is_empty() {
        bail!("{} error(s) occurred.", errors.len(),);
    }

    success!("Synchronized successfully.");
    Ok(())
}

async fn build_remote_snapshot(url: &Url) -> Result<Snapshot> {
    let res = reqwest::get(url.join("snapshot")?)
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
