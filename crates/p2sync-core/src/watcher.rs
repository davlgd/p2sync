use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Events emitted by the filesystem watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
}

/// Start watching a directory for changes.
/// Returns a receiver that yields `FsEvent`s and a guard that stops watching on drop.
pub fn watch(
    root: &Path,
    exclude: Vec<String>,
) -> notify::Result<(mpsc::Receiver<FsEvent>, WatchGuard)> {
    let (event_tx, rx) = mpsc::channel();
    let root_path = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };

            for path in &event.paths {
                let rel = match path.strip_prefix(&root_path) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                if should_skip(rel, &exclude) {
                    continue;
                }

                let fs_event = match event.kind {
                    EventKind::Create(_) => FsEvent::Created(rel.to_path_buf()),
                    EventKind::Modify(_) => FsEvent::Modified(rel.to_path_buf()),
                    EventKind::Remove(_) => FsEvent::Removed(rel.to_path_buf()),
                    _ => continue,
                };

                let _ = event_tx.send(fs_event);
            }
        },
        Config::default().with_poll_interval(Duration::from_secs(1)),
    )?;

    watcher.watch(root, RecursiveMode::Recursive)?;

    Ok((rx, WatchGuard { _watcher: watcher }))
}

/// Guard that keeps the watcher alive. Watching stops when dropped.
pub struct WatchGuard {
    _watcher: RecommendedWatcher,
}

fn should_skip(rel_path: &Path, exclude: &[String]) -> bool {
    rel_path.components().any(|c| {
        if let std::path::Component::Normal(s) = c {
            let name = s.to_string_lossy();
            exclude.iter().any(|pat| name.as_ref() == pat)
        } else {
            false
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;

    #[test]
    fn detects_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let (rx, _guard) = watch(dir.path(), vec![]).unwrap();

        thread::sleep(Duration::from_secs(1));
        fs::write(dir.path().join("new.txt"), b"hello").unwrap();

        let events = collect_events(&rx, Duration::from_secs(5));
        assert!(
            events.iter().any(|e| matches!(e, FsEvent::Created(p) | FsEvent::Modified(p) if p == Path::new("new.txt"))),
            "expected creation event, got: {events:?}"
        );
    }

    #[test]
    fn detects_file_modification() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), b"v1").unwrap();

        let (rx, _guard) = watch(dir.path(), vec![]).unwrap();
        thread::sleep(Duration::from_secs(1));

        fs::write(dir.path().join("f.txt"), b"v2").unwrap();

        let events = collect_events(&rx, Duration::from_secs(5));
        assert!(
            events.iter().any(|e| matches!(e, FsEvent::Modified(p) | FsEvent::Created(p) if p == Path::new("f.txt"))),
            "expected modify event, got: {events:?}"
        );
    }

    #[test]
    fn detects_file_removal() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), b"data").unwrap();

        let (rx, _guard) = watch(dir.path(), vec![]).unwrap();
        thread::sleep(Duration::from_secs(1));

        fs::remove_file(dir.path().join("f.txt")).unwrap();

        let events = collect_events(&rx, Duration::from_secs(5));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, FsEvent::Removed(p) if p == Path::new("f.txt"))),
            "expected remove event, got: {events:?}"
        );
    }

    #[test]
    fn excludes_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let (rx, _guard) = watch(dir.path(), vec![".git".into()]).unwrap();

        thread::sleep(Duration::from_secs(1));
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), b"ignored").unwrap();
        fs::write(dir.path().join("tracked.txt"), b"visible").unwrap();

        let events = collect_events(&rx, Duration::from_secs(5));
        assert!(
            events.iter().all(|e| {
                let path = match e {
                    FsEvent::Created(p) | FsEvent::Modified(p) | FsEvent::Removed(p) => p,
                };
                !path.starts_with(".git")
            }),
            "expected .git to be excluded, got: {events:?}"
        );
    }

    fn collect_events(rx: &mpsc::Receiver<FsEvent>, timeout: Duration) -> Vec<FsEvent> {
        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(e) => events.push(e),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !events.is_empty() {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        events
    }
}
