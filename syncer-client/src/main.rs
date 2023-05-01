#![forbid(unsafe_code)]
#![forbid(unused_must_use)]
#![warn(unused_crate_dependencies)]

mod cmd;
mod diffing;
mod logging;

use std::{future::Future, sync::atomic::Ordering, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use cmd::Args;
use colored::Colorize;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use reqwest::Url;
use syncer_common::{
    snapshot::{make_snapshot, Snapshot, SnapshotItemMetadata},
    PING_ANSWER,
};
use tokio::join;

use crate::{
    diffing::{build_diff, Diff},
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
        async_with_spinner(remote_pb, |_| build_remote_snapshot(url))
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

    let transfer_count = added.len() + modified.len() + type_changed.len();
    let delete_count = type_changed.len() + deleted.len();
    let transfer_size = added.iter().fold(0, |acc, (_, i)| {
        acc + match i.new {
            SnapshotItemMetadata::Directory => 0,
            SnapshotItemMetadata::File(mt) => mt.size,
        }
    }) + modified.iter().fold(0, |acc, (_, i)| acc + i.new.size)
        + type_changed.iter().fold(0, |acc, (_, i)| {
            acc + match i.new {
                SnapshotItemMetadata::Directory => 0,
                SnapshotItemMetadata::File(mt) => mt.size,
            }
        });

    info!(
        "Found a total of {} items to transfer and {} to delete for a total of {}.",
        transfer_count.to_string().bright_green(),
        delete_count.to_string().bright_red(),
        format!("{}", HumanBytes(transfer_size)).bright_yellow()
    );

    // TODO: actual transfer :p

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
