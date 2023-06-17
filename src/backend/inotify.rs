use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fmt::Display,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::{pin_mut, StreamExt};
use log::{debug, error};

use crate::{
    inotify::{normalize, INotify, INotifyMask, WatchHandle},
    FileWatcherError, WatcherContext,
};

pub(crate) async fn start_backend<E: Display + Send + 'static>(
    mut watcher_context: WatcherContext,
) {
    tokio::spawn(async move {
        watcher_context.file = normalize(&watcher_context.file);
        let watcher_context = Arc::new(watcher_context);
        loop {
            if let Err(e) = load_config::<E>(watcher_context.clone()).await {
                error!(
                    "{} watch error: {e} @ '{}'",
                    watcher_context.log_name,
                    watcher_context.file.display()
                );
                tokio::time::sleep(watcher_context.retry_interval).await;
            }
        }
    });
}

const MAX_ITER: usize = 16;

pub(crate) async fn load_config<E: Display + Send + 'static>(
    context: Arc<WatcherContext>,
) -> Result<(), FileWatcherError<E>> {
    let mut notify = INotify::new()?;
    let mut watch_handles = vec![];
    let mut interesting_children: HashMap<WatchHandle, OsString> = HashMap::new();
    let mut symlinks: HashSet<WatchHandle> = HashSet::new();
    let mut current_main_file = context.file.clone();
    let mut hanging_dirs = vec![];
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    loop {
        debug!(
            "watching main target or link {}",
            current_main_file.display()
        );
        let main_notify = notify.add_watch(
            &current_main_file,
            INotifyMask::CloseWrite
                | INotifyMask::DeleteSelf
                | INotifyMask::Modify
                | INotifyMask::MoveSelf
                | INotifyMask::DontFollow,
        )?;
        watch_handles.push(main_notify);
        if let Some(parent) = current_main_file.parent() {
            hanging_dirs.push((
                parent.to_path_buf(),
                Some(current_main_file.file_name().unwrap().to_os_string()),
            ));
        }
        let main_file_metadata = tokio::fs::symlink_metadata(&current_main_file).await?;
        if main_file_metadata.is_symlink() {
            symlinks.insert(main_notify);
            let link = tokio::fs::read_link(&current_main_file).await?;
            current_main_file = if link.is_relative() {
                current_main_file.parent().unwrap().join(link)
            } else {
                link
            };
            current_main_file = normalize(&current_main_file);
        } else {
            break;
        }
    }
    let mut next_round = hanging_dirs;
    let mut round_count = 0usize;
    loop {
        let mut round = std::mem::take(&mut next_round);
        if round.is_empty() || round_count > MAX_ITER {
            break;
        }
        round_count += 1;
        while let Some((dir, child)) = round.pop() {
            if seen_dirs.contains(&dir) {
                continue;
            }
            let dir_metadata = tokio::fs::symlink_metadata(&dir).await?;
            debug!("watching ancestor {}", dir.display());
            if dir_metadata.is_symlink() {
                let mut link = tokio::fs::read_link(&dir).await?;
                if link.is_relative() {
                    link = dir.parent().unwrap().join(link);
                }
                link = normalize(&link);
                next_round.push((link, child));
                watch_handles.push(notify.add_watch(
                    &dir,
                    INotifyMask::CloseWrite
                        | INotifyMask::DeleteSelf
                        | INotifyMask::Modify
                        | INotifyMask::MoveSelf
                        | INotifyMask::DontFollow,
                )?);
            } else {
                let watcher = notify.add_watch(
                    &dir,
                    INotifyMask::Delete
                        | INotifyMask::DeleteSelf
                        | INotifyMask::Modify
                        | INotifyMask::MoveSelf
                        | INotifyMask::MovedFrom
                        | INotifyMask::MovedTo
                        | INotifyMask::DontFollow,
                )?;
                watch_handles.push(watcher);
                interesting_children
                    .insert(watcher, child.expect("missing child for non-symlink root"));
            }
            seen_dirs.insert(dir.clone());
            let mut current_child: &Path = &dir;
            let mut current_parent = current_child.parent();
            while let Some(parent) = current_parent {
                if seen_dirs.contains(parent) {
                    break;
                }
                debug!("watching ancestor {}", parent.display());
                let metadata = tokio::fs::symlink_metadata(parent).await?;
                if metadata.is_symlink() {
                    let mut link = tokio::fs::read_link(parent).await?;
                    if link.is_relative() {
                        link = dir.parent().unwrap().join(link);
                    }
                    link = normalize(&link);
                    next_round.push((
                        link,
                        Some(current_child.file_name().unwrap().to_os_string()),
                    ));
                    watch_handles.push(notify.add_watch(
                        parent,
                        INotifyMask::CloseWrite
                            | INotifyMask::DeleteSelf
                            | INotifyMask::Modify
                            | INotifyMask::MoveSelf
                            | INotifyMask::DontFollow,
                    )?);
                } else {
                    let watcher = notify.add_watch(
                        parent,
                        INotifyMask::Delete
                            | INotifyMask::DeleteSelf
                            | INotifyMask::Modify
                            | INotifyMask::MoveSelf
                            | INotifyMask::MovedFrom
                            | INotifyMask::MovedTo
                            | INotifyMask::DontFollow,
                    )?;
                    watch_handles.push(watcher);
                    interesting_children
                        .insert(watcher, current_child.file_name().unwrap().to_os_string());
                }
                seen_dirs.insert(parent.to_path_buf());

                current_child = parent;
                current_parent = parent.parent();
            }
        }
    }

    let stream = notify.stream();
    pin_mut!(stream);
    while let Some(event) = stream.next().await {
        let event = match event {
            Err(e) => {
                return Err(e.into());
            }
            Ok(x) => x,
        };
        debug!("received event {event:?}");
        if let Some(interest) = interesting_children.get(&event.watch_descriptor) {
            // a directory event we need to filter, and if applicable, always full refresh
            if &event.name != interest {
                continue;
            }
            context.notify.notify_one();

            return Ok(());
        } else if symlinks.contains(&event.watch_descriptor) {
            // a symlink changed, we always reload and need a full refresh
            context.notify.notify_one();
            return Ok(());
        } else {
            // the underlying file was modified, we don't need to full refresh
            context.notify.notify_one();
        }
    }
    Ok(())
}
