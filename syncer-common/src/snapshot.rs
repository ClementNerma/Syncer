use std::{
    ffi::OsStr,
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

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct SnapshotOptions {
    pub ignore_paths: Vec<String>,
    pub ignore_names: Vec<String>,
    pub ignore_exts: Vec<String>,
}

impl SnapshotOptions {
    pub fn validate(&self) -> Result<()> {
        for path in &self.ignore_paths {
            if Path::new(path).is_absolute() {
                bail!("Paths to ignore must be relative (got '{path}')",);
            }
        }

        for name in &self.ignore_names {
            if name.contains('/') || name.contains('\\') {
                bail!(
                    "Names to ignore must not contain a path separator (got '{}')",
                    name
                );
            }
        }

        for ext in &self.ignore_exts {
            if ext.contains('.') {
                bail!(
                    "Extensions to ignore must not contain a dot (got '{}')",
                    ext
                );
            }
        }

        Ok(())
    }

    pub fn should_ignore(&self, relative_path: &Path, from_dir: &Path) -> bool {
        if self
            .ignore_paths
            .iter()
            .any(|c| relative_path == Path::new(c))
        {
            return true;
        }

        if let Some(file_name) = relative_path.file_name() {
            if self.ignore_names.iter().any(|c| OsStr::new(c) == file_name) {
                return true;
            }
        }

        if from_dir.join(relative_path).is_file() {
            if let Some(ext) = relative_path.extension() {
                if self.ignore_exts.iter().any(|c| OsStr::new(c) == ext) {
                    return true;
                }
            }
        }

        false
    }
}

#[derive(Serialize, Deserialize)]
pub struct SnapshotResult {
    pub snapshot: Snapshot,
    pub debug: Vec<String>,
}

pub async fn make_snapshot(
    from_dir: PathBuf,
    progress: impl Fn(String) + Send + Sync + 'static,
    options: &SnapshotOptions,
) -> Result<SnapshotResult> {
    let total = Arc::new(Mutex::new(AtomicUsize::new(0)));
    let progress = Arc::new(RwLock::new(progress));

    let mut debug = vec![];

    let mut items = Vec::new();

    for item in WalkDir::new(&from_dir).min_depth(1) {
        let item = item?;

        let from = from_dir.clone();

        let progress = Arc::clone(&progress);
        let total = Arc::clone(&total);

        let path = item.path();

        let relative_path = path.strip_prefix(&from_dir).unwrap();

        if options.should_ignore(relative_path, &from_dir) {
            debug.push(format!(
                "Ignored item based on provided options: {}",
                path.strip_prefix(&from_dir).unwrap().display()
            ));
        } else {
            let item = snapshot_item(path, &from).await.with_context(|| {
                format!("Failed analysis on filesystem item: {}", path.display())
            })?;

            items.push(item);
        }

        let total = total.lock().await.fetch_add(1, Ordering::Release) + 1;

        (progress.read().await)(format!("Analyzed {total} item(s)"));
    }

    let from_dir_str = from_dir.to_str().with_context(|| {
        format!(
            "Provided path contains invalid UTF-8 characters: {}",
            from_dir.display()
        )
    })?;

    Ok(SnapshotResult {
        snapshot: Snapshot {
            from_dir: from_dir_str.to_string(),
            items,
        },
        debug,
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
