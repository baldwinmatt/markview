use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::{resolve, snapshot, store_revision};

#[derive(Default)]
struct DocumentWatch {
    targets: HashSet<PathBuf>,
    fingerprint: String,
}

impl DocumentWatch {
    fn inspect(path: &Path) -> Self {
        let mut targets = HashSet::from([path.to_owned()]);
        if let Some(parent) = path.parent() {
            for ancestor in parent.ancestors() {
                targets.insert(ancestor.join(".git"));
                targets.insert(ancestor.join(".markview"));
                targets.insert(ancestor.join(".markview/annotations"));
            }
        }
        let fingerprint = match resolve(path) {
            Ok(location) => {
                let store = location.root.join(".markview/annotations");
                for component in location.path.ancestors().take_while(|path| *path != store) {
                    targets.insert(component.to_owned());
                }
                match snapshot(&location) {
                    Ok(bytes) => store_revision(&location.path, bytes.as_deref()),
                    Err(error) => error.to_string(),
                }
            }
            Err(error) => error.to_string(),
        };
        Self {
            targets,
            fingerprint,
        }
    }
}

pub struct StoreWatcher {
    watcher: RecommendedWatcher,
    directories: HashSet<PathBuf>,
    documents: HashMap<PathBuf, DocumentWatch>,
}

impl StoreWatcher {
    pub fn new(
        on_event: impl FnMut(notify::Result<Event>) + Send + 'static,
    ) -> notify::Result<Self> {
        Ok(Self {
            watcher: notify::recommended_watcher(on_event)?,
            directories: HashSet::new(),
            documents: HashMap::new(),
        })
    }

    pub fn sync(&mut self, documents: impl IntoIterator<Item = PathBuf>) {
        let next: HashSet<_> = documents.into_iter().collect();
        if next.len() == self.documents.len()
            && next.iter().all(|path| self.documents.contains_key(path))
        {
            return;
        }
        self.documents.retain(|path, _| next.contains(path));
        for path in next {
            self.documents
                .entry(path.clone())
                .or_insert_with(|| DocumentWatch::inspect(&path));
        }
        self.sync_directories();
    }

    fn sync_directories(&mut self) {
        let next: HashSet<_> = self
            .documents
            .values()
            .flat_map(|doc| &doc.targets)
            .filter_map(|path| path.parent())
            .filter(|path| {
                fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
                    && path
                        .canonicalize()
                        .is_ok_and(|canonical| canonical == *path)
            })
            .map(Path::to_owned)
            .collect();
        for path in self.directories.difference(&next) {
            let _ = self.watcher.unwatch(path);
        }
        self.directories.retain(|path| next.contains(path));
        for path in next {
            if !self.directories.contains(&path)
                && self
                    .watcher
                    .watch(&path, RecursiveMode::NonRecursive)
                    .is_ok()
            {
                self.directories.insert(path);
            }
        }
    }

    pub fn changed(&mut self, events: &[Event]) -> Vec<PathBuf> {
        let paths: Vec<_> = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Any
                )
            })
            .flat_map(|event| event.paths.iter())
            .collect();
        let mut changed = Vec::new();
        let mut replan = false;
        for (document, watch) in &mut self.documents {
            if paths.iter().any(|path| watch.targets.contains(*path)) {
                let next = DocumentWatch::inspect(document);
                if next.fingerprint != watch.fingerprint {
                    changed.push(document.clone());
                }
                *watch = next;
                replan = true;
            }
        }
        if replan {
            // A directory can be removed and recreated in one event batch.
            // Reinstall its watch even when the planned path is unchanged.
            self.directories.retain(|dir| {
                if paths.contains(&dir) {
                    let _ = self.watcher.unwatch(dir);
                    false
                } else {
                    true
                }
            });
            self.sync_directories();
        }
        changed
    }
}
