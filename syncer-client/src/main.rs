#![forbid(unsafe_code)]
#![forbid(unused_must_use)]
#![warn(unused_crate_dependencies)]

mod cmd;
mod diffing;
mod logging;

use std::{error, future::Future, sync::atomic::Ordering, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use cmd::Args;
use colored::Colorize;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use reqwest::{Client, Url};
use syncer_common::{
    snapshot::{make_snapshot, Snapshot, SnapshotFileMetadata, SnapshotItemMetadata},
    PING_ANSWER,
};
use tokio::join;

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
        async_with_spinner(remote_pb, |_| build_remote_snapshot(url.clone()))
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

    info!(
        "Found a total of {} files (+ {} dirs) to transfer and {} to delete for a total of {}.",
        to_transfer.len().to_string().bright_green(),
        to_create_dirs.len().to_string().bright_green(),
        to_delete.len().to_string().bright_red(),
        format!(
            "{}",
            HumanBytes(to_transfer.iter().map(|(_, mt)| mt.size).sum())
        )
        .bright_yellow()
    );

    // TODO: actual transfer :p

    // added
    // modified
    // type_changed
    // deleted

    let mut errors = vec![];

    let mp = MultiProgress::new();

    let delete_pb = mp.add(ProgressBar::new(to_delete.len() as u64));
    let create_dirs_pb = mp.add(ProgressBar::new(to_create_dirs.len() as u64));
    let transfer_pb = mp.add(ProgressBar::new(to_transfer.len() as u64));

    for (path, item) in &to_delete {
        let uri_item_type = match item {
            SnapshotItemMetadata::Directory => "dir",
            SnapshotItemMetadata::File(_) => "file",
        };

        let req = Client::new()
            .delete(&format!("{url}/fs/{uri_item_type}/delete",))
            .query(&[("path", path)]);

        if let Err(err) = req.send().await {
            errors.push(format!("Failed to delete file '{path}': {err}"));
        }

        delete_pb.inc(1);
    }

    for path in &to_create_dirs {
        let req = Client::new()
            .put(&format!("{url}/fs/dir/create"))
            .query(&[("path", path)]);

        if let Err(err) = req.send().await {
            errors.push(format!("Failed to create directory '{path}': {err}"));
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
        let req = Client::new()
            .put(&format!("{url}/fs/file/write"))
            .query(&[("path", path)]);

        if let Err(err) = req.send().await {
            errors.push(format!("Failed to transfer file '{path}': {err}"));
        }

        transfer_pb.inc(1);

        // TODO: handle last_modif_date, last_modif_date_ns, and size
        // used by the server to assert everything is valid + write timestamps
    }

    if !errors.is_empty() {
        bail!(
            "{} error(s) occurred:\n{}",
            errors.len(),
            errors
                .iter()
                .map(|err| format!("\n* {err}"))
                .collect::<String>()
        );
    }

    success!("Synchronized successfully.");
    Ok(())
}

async fn build_remote_snapshot(mut url: Url) -> Result<Snapshot> {
    url.path_segments_mut()
        .map_err(|()| anyhow!("Provided URL cannot be used as a base"))?
        .push("snapshot");

    let res = reqwest::get(url)
        .await
        .context("Failed to get remote snapshot")?;

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
