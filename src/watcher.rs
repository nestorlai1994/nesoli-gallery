use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::SystemTime,
};
use tokio::sync::mpsc::Sender;

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "raw", "cr2", "cr3", "nef", "arw", "dng", "raf", "orf", "rw2",
];

#[derive(Debug, Clone)]
pub enum WatchEventKind {
    Created,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
}

#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub path: PathBuf,
    pub kind: WatchEventKind,
    pub detected_at: SystemTime,
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Start a blocking file system watcher on `dir`.
/// Sends `WatchEvent`s to `tx` for image files only.
/// Intended to run inside `tokio::task::spawn_blocking`.
pub fn start_watcher(dir: PathBuf, tx: Sender<WatchEvent>) {
    let (sync_tx, sync_rx) = mpsc::channel();

    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<Event>| {
            if let Ok(event) = result {
                let _ = sync_tx.send(event);
            }
        },
        Config::default(),
    )
    .expect("failed to create file watcher");

    watcher
        .watch(&dir, RecursiveMode::Recursive)
        .expect("failed to watch directory");

    tracing::info!("watching {:?} for image files", dir);

    for event in sync_rx {
        let detected_at = SystemTime::now();

        let watch_event = match event.kind {
            EventKind::Create(_) => event.paths.into_iter().filter(|p| is_image(p)).map(|p| {
                WatchEvent { path: p, kind: WatchEventKind::Created, detected_at }
            }).collect::<Vec<_>>(),

            // Rename must come before Modify(_) — it's a ModifyKind::Name variant
            EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::Both))
                if event.paths.len() == 2 =>
            {
                let from = event.paths[0].clone();
                let to   = event.paths[1].clone();
                if is_image(&to) {
                    vec![WatchEvent {
                        path: to,
                        kind: WatchEventKind::Renamed { from },
                        detected_at,
                    }]
                } else {
                    vec![]
                }
            }

            EventKind::Modify(_) => event.paths.into_iter().filter(|p| is_image(p)).map(|p| {
                WatchEvent { path: p, kind: WatchEventKind::Modified, detected_at }
            }).collect::<Vec<_>>(),

            EventKind::Remove(_) => event.paths.into_iter().filter(|p| is_image(p)).map(|p| {
                WatchEvent { path: p, kind: WatchEventKind::Deleted, detected_at }
            }).collect::<Vec<_>>(),

            _ => vec![],
        };

        for ev in watch_event {
            if tx.blocking_send(ev).is_err() {
                tracing::warn!("watcher channel closed, stopping watcher");
                return;
            }
        }
    }
}
