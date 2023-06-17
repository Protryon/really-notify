use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use backend::start_backend;
use log::{error, info};
use thiserror::Error;
use tokio::{
    select,
    sync::{mpsc, Notify},
};

mod backend;
#[cfg(all(feature = "inotify", target_family = "unix"))]
mod inotify;

/// `really-notify` primary input.
/// [`T`] is the target parse type, i.e. your serde-deserializable `Config` struct.
/// [`E`] is the generic error type that your parser can fail with.
pub struct FileWatcherConfig<T, E> {
    /// Cosmetic, used for logs to be consistent with application terminology
    pub log_name: String,
    /// Path to the file you are interested in changes of. Do your worse with symlinks here.
    pub file: PathBuf,
    /// Parser function to transform a modified target file into our desired output. If you just want raw bytes, you can pass it through, or not set this at all.
    pub parser: Arc<dyn Fn(Vec<u8>) -> Result<T, E> + Send + Sync>,
    /// Defaults to one second, how often to attempt reparsing/error recovery.
    pub retry_interval: Duration,
}

#[derive(Error, Debug)]
enum FileWatcherError<E: Display> {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[cfg(feature = "notify")]
    #[error("{0}")]
    Notify(#[from] notify::Error),
    #[error("{0}")]
    Parse(E),
}

pub(crate) struct WatcherContext {
    pub(crate) file: PathBuf,
    pub(crate) log_name: String,
    pub(crate) retry_interval: Duration,
    pub(crate) notify: Arc<Notify>,
}

/// Impossible to fail converting a Vec<u8> to a Vec<u8>
pub enum Infallible {}

impl fmt::Display for Infallible {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        unreachable!()
    }
}

impl FileWatcherConfig<Vec<u8>, Infallible> {
    pub fn new(file: impl AsRef<Path>, log_name: impl AsRef<str>) -> Self {
        Self {
            file: file.as_ref().to_path_buf(),
            log_name: log_name.as_ref().to_string(),
            parser: Arc::new(|x| Ok(x)),
            retry_interval: Duration::from_secs(1),
        }
    }
}

impl<T: Send + 'static, E: Display + Send + 'static> FileWatcherConfig<T, E> {
    /// Set a new parser and adjust the FileWatcherConfig type parameters as needed.
    pub fn with_parser<T2: Send + 'static, E2: Display + Send + 'static>(
        self,
        func: impl Fn(Vec<u8>) -> Result<T2, E2> + Send + Sync + 'static,
    ) -> FileWatcherConfig<T2, E2> {
        FileWatcherConfig {
            log_name: self.log_name,
            file: self.file,
            parser: Arc::new(func),
            retry_interval: self.retry_interval,
        }
    }

    /// Set an alternative retry_interval
    pub fn with_retry_interval(mut self, retry_interval: Duration) -> Self {
        self.retry_interval = retry_interval;
        self
    }

    /// Run the watcher. Dropping/closing this receiver will cause an immediate cleanup.
    pub fn start(self) -> mpsc::Receiver<T> {
        let (sender, receiver) = mpsc::channel(3);
        tokio::spawn(self.run(sender));
        receiver
    }

    async fn run(self, sender: mpsc::Sender<T>) {
        let target = loop {
            match self.read_target().await {
                Ok(x) => break x,
                Err(e) => {
                    error!(
                        "failed to read initial {}: {e} @ '{}', retrying in {:.1} second(s)",
                        self.log_name,
                        self.file.display(),
                        self.retry_interval.as_secs_f64(),
                    );
                    tokio::time::sleep(self.retry_interval).await;
                }
            }
        };
        if sender.send(target).await.is_err() {
            return;
        }
        let mut file = self.file.clone();
        if file.is_relative() {
            if let Ok(cwd) = std::env::current_dir() {
                file = cwd.join(file);
            }
        }
        let notify = Arc::new(Notify::new());
        let watcher_context = WatcherContext {
            file,
            log_name: self.log_name.clone(),
            retry_interval: self.retry_interval,
            notify: notify.clone(),
        };
        start_backend::<E>(watcher_context).await;
        loop {
            select! {
                _ = notify.notified() => {
                    let target = loop {
                        match self.read_target().await {
                            Ok(x) => break x,
                            Err(e) => {
                                error!("failed to read {} update: {e} @ {}, retrying in {:.1} second(s)", self.log_name, self.file.display(), self.retry_interval.as_secs_f64());
                                tokio::time::sleep(self.retry_interval).await;
                                // toss out any pending notification, since we will already try again
                                let notify = notify.notified();
                                futures::pin_mut!(notify);
                                notify.enable();
                            }
                        }
                    };
                    if sender.send(target).await.is_err() {
                        return;
                    }
                },
                _ = sender.closed() => {
                    return;
                }
            }
        }
    }

    async fn read_target(&self) -> Result<T, FileWatcherError<E>> {
        info!(
            "reading updated {} '{}'",
            self.log_name,
            self.file.display()
        );
        let raw = tokio::fs::read(&self.file).await?;
        (self.parser)(raw).map_err(FileWatcherError::Parse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_file_zone() {
        env_logger::Builder::new()
            .parse_env(env_logger::Env::default().default_filter_or("info"))
            .init();
        let mut receiver = FileWatcherConfig::new("./test.yaml", "config").start();
        while let Some(_update) = receiver.recv().await {
            println!("updated!");
        }
    }
}
