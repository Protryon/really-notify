use std::{fmt::Display, sync::Arc};

use log::{debug, error, info};
use notify::{
    event::{AccessKind, AccessMode},
    EventKind, RecursiveMode, Watcher,
};
use tokio::sync::oneshot;

use crate::{FileWatcherError, WatcherContext};

pub(crate) async fn start_backend<E: Display + Send + 'static>(watcher_context: WatcherContext) {
    tokio::task::spawn_blocking(move || {
        let watcher_context = Arc::new(watcher_context);
        loop {
            match load_config::<E>(watcher_context.clone()) {
                Ok(()) => break,
                Err(e) => {
                    error!(
                        "failed to setup {} watcher: {e} @ '{}', retrying in {:.1} second(s)",
                        watcher_context.log_name,
                        watcher_context.file.display(),
                        watcher_context.retry_interval.as_secs_f64()
                    );
                    std::thread::sleep(watcher_context.retry_interval);
                }
            }
        }
    })
    .await
    .unwrap();
}

fn load_config<E: Display + Send + 'static>(
    context: Arc<WatcherContext>,
) -> Result<(), FileWatcherError<E>> {
    let (watcher_sender, watcher_receiver) = oneshot::channel();
    let mut watcher_receiver = Some(watcher_receiver);

    let context2 = context.clone();
    let realpath = std::fs::canonicalize(&context2.file)?;
    let realpath2 = realpath.clone();

    let mut watcher = notify::recommended_watcher(
        move |res: Result<notify::Event, notify::Error>| {
            if watcher_receiver.is_none() {
                return;
            }
            match res {
                Ok(event) => {
                    match event.kind {
                        EventKind::Access(AccessKind::Close(AccessMode::Write))
                        | EventKind::Modify(_)
                        | EventKind::Remove(_) => (),
                        _ => return,
                    }
                    let mut found_path = false;
                    for path in &event.paths {
                        if context
                            .file
                            .ancestors()
                            .chain(realpath2.ancestors())
                            .any(|x| x == path)
                        {
                            found_path = true;
                            break;
                        }
                    }
                    if !found_path {
                        return;
                    }
                    debug!("file updated: {:?}", event.paths);
                    context.notify.notify_one();
                    watcher_receiver.take().unwrap().blocking_recv().ok();
                    while let Err(e) = load_config::<E>(context.clone()) {
                        error!("failed to reload {} watcher: {e} @ '{}', retrying in {:.1} second(s)...", context.log_name, context.file.display(), context.retry_interval.as_secs_f64());
                        std::thread::sleep(context.retry_interval);
                        context.notify.notify_one();
                    }
                }
                Err(e) => {
                    error!(
                        "{} watch error: {e} @ '{}'",
                        context.log_name,
                        context.file.display()
                    );
                }
            }
        },
    )?;
    for ancestor in context2.file.ancestors() {
        debug!("watching {}", ancestor.display());
        watcher.watch(&ancestor, RecursiveMode::NonRecursive)?;
    }
    for ancestor in realpath.ancestors() {
        debug!("watching {}", ancestor.display());
        watcher.watch(&ancestor, RecursiveMode::NonRecursive)?;
    }
    watcher_sender.send(watcher).ok();

    Ok(())
}
