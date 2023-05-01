use syncer_common::snapshot::{Snapshot, SnapshotFileMetadata, SnapshotItem, SnapshotItemMetadata};

use std::collections::{HashMap, HashSet};

pub struct Diff {
    pub added: Vec<(String, DiffItemAdded)>,
    pub modified: Vec<(String, DiffItemModified)>,
    pub type_changed: Vec<(String, DiffItemTypeChanged)>,
    pub deleted: Vec<(String, DiffItemDeleted)>,
}

impl Diff {
    pub fn new(items: Vec<DiffItem>) -> Self {
        let mut added = vec![];
        let mut modified = vec![];
        let mut type_changed = vec![];
        let mut deleted = vec![];

        for item in items {
            match item.status {
                DiffType::Added(i) => added.push((item.path, i)),
                DiffType::Modified(i) => modified.push((item.path, i)),
                DiffType::TypeChanged(i) => type_changed.push((item.path, i)),
                DiffType::Deleted(i) => deleted.push((item.path, i)),
            }
        }

        Self {
            added,
            modified,
            type_changed,
            deleted,
        }
    }
}

pub struct DiffItem {
    pub status: DiffType,
    pub path: String,
}

pub enum DiffType {
    Added(DiffItemAdded),
    Modified(DiffItemModified),
    TypeChanged(DiffItemTypeChanged), // File => Dir / Dir => File
    Deleted(DiffItemDeleted),
}

pub struct DiffItemAdded {
    pub new: SnapshotItemMetadata,
}

pub struct DiffItemModified {
    pub prev: SnapshotFileMetadata,
    pub new: SnapshotFileMetadata,
}

pub struct DiffItemTypeChanged {
    pub prev: SnapshotItemMetadata,
    pub new: SnapshotItemMetadata,
}

pub struct DiffItemDeleted {
    pub prev: SnapshotItemMetadata,
}

pub fn build_diff(local: &Snapshot, remote: &Snapshot) -> Diff {
    let source_items = build_item_names_hashmap(local);
    let backed_up_items = build_item_names_hashmap(remote);

    let source_items_paths: HashSet<_> = source_items.keys().collect();
    let backed_up_items_paths: HashSet<_> = backed_up_items.keys().collect();

    let mut diff = Vec::with_capacity(source_items.len());

    // debug!("> Building list of new items...");

    diff.extend(
        source_items_paths
            .difference(&backed_up_items_paths)
            .map(|item| DiffItem {
                path: item.to_string(),
                status: DiffType::Added(DiffItemAdded {
                    new: source_items.get(*item).unwrap().metadata,
                }),
            }),
    );

    // debug!("> Building list of deleted items...");

    diff.extend(
        backed_up_items_paths
            .difference(&source_items_paths)
            .map(|item| DiffItem {
                path: item.to_string(),
                status: DiffType::Deleted(DiffItemDeleted {
                    prev: backed_up_items.get(*item).unwrap().metadata,
                }),
            }),
    );

    // debug!("> Building list of modified items...");

    diff.extend(
        local
            .items
            .iter()
            .filter(|item| backed_up_items_paths.contains(&item.relative_path.as_str()))
            .filter_map(|source_item| {
                let backed_up_item = backed_up_items
                    .get(&source_item.relative_path.as_str())
                    .unwrap();

                match (source_item.metadata, backed_up_item.metadata) {
                    // Both directories = no change
                    (SnapshotItemMetadata::Directory, SnapshotItemMetadata::Directory) => None,
                    // Source item is directory and backed up item is file or the opposite = type changed
                    (SnapshotItemMetadata::Directory, SnapshotItemMetadata::File { .. })
                    | (SnapshotItemMetadata::File { .. }, SnapshotItemMetadata::Directory) => {
                        Some(DiffItem {
                            path: source_item.relative_path.clone(),
                            status: DiffType::TypeChanged(DiffItemTypeChanged {
                                prev: backed_up_item.metadata,
                                new: source_item.metadata,
                            }),
                        })
                    }
                    // Otherwise, compare their metadata to see if something changed
                    (
                        SnapshotItemMetadata::File(source_data),
                        SnapshotItemMetadata::File(backed_up_data),
                    ) => {
                        if source_data == backed_up_data {
                            None
                        } else {
                            Some(DiffItem {
                                path: source_item.relative_path.clone(),
                                status: DiffType::Modified(DiffItemModified {
                                    prev: backed_up_data,
                                    new: source_data,
                                }),
                            })
                        }
                    }
                }
            }),
    );

    diff.sort_by(|a, b| a.path.cmp(&b.path));

    Diff::new(diff)
}

fn build_item_names_hashmap(snapshot: &Snapshot) -> HashMap<&str, &SnapshotItem> {
    snapshot
        .items
        .iter()
        .map(|item| (item.relative_path.as_str(), item))
        .collect::<HashMap<_, _>>()
}
