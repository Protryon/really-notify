#[cfg(all(
    feature = "notify",
    not(all(feature = "inotify", target_family = "unix"))
))]
mod notify;
#[cfg(all(
    feature = "notify",
    not(all(feature = "inotify", target_family = "unix"))
))]
pub(crate) use self::notify::*;

#[cfg(all(feature = "inotify", target_family = "unix"))]
mod inotify;
#[cfg(all(feature = "inotify", target_family = "unix"))]
pub(crate) use inotify::*;
