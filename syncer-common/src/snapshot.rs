use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::SystemTime,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use walkdir::WalkDir;

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub from_dir: String,
    pub items: Vec<SnapshotItem>,
}

#[derive(Serialize, Deserialize)]
pub struct SnapshotItem {
    pub relative_path: String,
    pub metadata: SnapshotItemMetadata,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum SnapshotItemMetadata {
    Directory,
    File(SnapshotFileMetadata),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotFileMetadata {
    pub size: u64,
    pub last_modif_date: u64,
    pub last_modif_date_ns: u32,
}

pub async fn make_snapshot(
    from_dir: PathBuf,
    progress: impl Fn(String) + Send + Sync + 'static,
) -> Result<Snapshot> {
    let total = Arc::new(Mutex::new(AtomicUsize::new(0)));
    let progress = Arc::new(RwLock::new(progress));

    let mut items = Vec::new();

    for item in WalkDir::new(&from_dir).min_depth(1) {
        let item = item?;

        let from = from_dir.clone();

        let progress = Arc::clone(&progress);
        let total = Arc::clone(&total);

        let path = item.path();

        let item = snapshot_item(path, &from)
            .await
            .with_context(|| format!("Failed analysis on filesystem item: {}", path.display()))?;

        items.push(item);

        let total = total.lock().await.fetch_add(1, Ordering::Release) + 1;

        (progress.read().await)(format!("Analyzed {total} item(s)"));
    }

    let from_dir_str = from_dir.to_str().with_context(|| {
        format!(
            "Provided path contains invalid UTF-8 characters: {}",
            from_dir.display()
        )
    })?;

    Ok(Snapshot {
        from_dir: from_dir_str.to_string(),
        items,
    })
}

async fn snapshot_item(item: &Path, from: &Path) -> Result<SnapshotItem> {
    let metadata = item.metadata()?;

    if metadata.is_symlink() {
        bail!("Symbolc links are unsupported.");
    }

    let metadata = if metadata.is_dir() {
        SnapshotItemMetadata::Directory
    } else if metadata.is_file() {
        let mtime = metadata
            .modified()
            .with_context(|| {
                format!(
                    "Failed to get modification time of file: {}",
                    item.display()
                )
            })?
            .duration_since(SystemTime::UNIX_EPOCH)
            .with_context(|| {
                format!(
                    "Found invalid modification time for file: {}",
                    item.display()
                )
            })?;

        SnapshotItemMetadata::File(SnapshotFileMetadata {
            size: metadata.len(),
            last_modif_date: mtime.as_secs(),
            last_modif_date_ns: mtime.subsec_nanos(),
        })
    } else {
        bail!("Unknown item type (not a symlink, file nor directory)");
    };

    let relative_path = item.strip_prefix(from).unwrap().to_path_buf();

    let relative_path_str = relative_path.to_str().with_context(|| {
        format!(
            "Relative path contains invalid UTF-8 characters: {}",
            relative_path.display()
        )
    })?;

    Ok(SnapshotItem {
        relative_path: relative_path_str.to_string(),
        metadata,
    })
}
