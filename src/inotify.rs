use std::{
    ffi::{CString, OsString},
    fs::File,
    io::Error as IoError,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::prelude::{OsStrExt, OsStringExt},
    },
    path::{Component, Path, PathBuf},
};

use async_stream::stream;
use bitmask_enum::bitmask;
use futures::Stream;
use log::debug;
use tokio::{io::AsyncReadExt, net::unix::pipe::Receiver};

pub struct INotify {
    stream: Receiver,
}

#[bitmask(u32)]
pub enum INotifyMask {
    Access = libc::IN_ACCESS,
    AttributeChanged = libc::IN_ATTRIB,
    CloseWrite = libc::IN_CLOSE_WRITE,
    CloseNoWrite = libc::IN_CLOSE_NOWRITE,
    Create = libc::IN_CREATE,
    Delete = libc::IN_DELETE,
    DeleteSelf = libc::IN_DELETE_SELF,
    Modify = libc::IN_MODIFY,
    MoveSelf = libc::IN_MOVE_SELF,
    MovedFrom = libc::IN_MOVED_FROM,
    MovedTo = libc::IN_MOVED_TO,
    Open = libc::IN_OPEN,
    // meta mask flags for create only
    DontFollow = libc::IN_DONT_FOLLOW,
    ExclUnlink = libc::IN_EXCL_UNLINK,
    MaskAdd = libc::IN_MASK_ADD,
    Oneshot = libc::IN_ONESHOT,
    OnlyDir = libc::IN_ONLYDIR,
    MaskCreate = libc::IN_MASK_CREATE,
    // meta mask flags only returned in read
    Ignored = libc::IN_IGNORED,
    IsDir = libc::IN_ISDIR,
    QueueOverflow = libc::IN_Q_OVERFLOW,
    Unmount = libc::IN_UNMOUNT,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct RawINotifyEvent {
    watch_descriptor: WatchHandle,
    mask: INotifyMask,
    cookie: u32,
    len: u32,
    // inline CString
}

const EVENT_SIZE: usize = std::mem::size_of::<RawINotifyEvent>();

#[derive(Debug)]
pub struct INotifyEvent {
    pub watch_descriptor: WatchHandle,
    pub mask: INotifyMask,
    pub cookie: u32,
    pub name: OsString,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct WatchHandle(i32);

pub(crate) fn normalize(path: &Path) -> PathBuf {
    if !path.is_absolute() {
        panic!("attempted to normalize a relative path");
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => unreachable!(),
            Component::RootDir => out.push("/"),
            Component::CurDir => out.push("."),
            Component::ParentDir => out.push(".."),
            Component::Normal(component) => out.push(component),
        }
    }
    out
}

const NAME_MAX: usize = 255;

impl INotify {
    pub fn new() -> Result<Self, IoError> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(IoError::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let stream = Receiver::from_file_unchecked(file)?;
        Ok(Self { stream })
    }

    pub fn add_watch(
        &self,
        path: impl AsRef<Path>,
        mask: INotifyMask,
    ) -> Result<WatchHandle, IoError> {
        let pathd = path.as_ref();
        let path = CString::new(path.as_ref().as_os_str().as_bytes()).expect("NUL byte in path");
        let descriptor =
            unsafe { libc::inotify_add_watch(self.stream.as_raw_fd(), path.as_ptr(), mask.bits()) };
        if descriptor < 0 {
            return Err(IoError::last_os_error());
        }
        debug!("watching {descriptor}: {} {mask:?}", pathd.display());
        Ok(WatchHandle(descriptor))
    }

    #[allow(dead_code)]
    pub fn rm_watch(&self, handle: WatchHandle) -> Result<(), IoError> {
        let out = unsafe { libc::inotify_rm_watch(self.stream.as_raw_fd(), handle.0) };
        if out < 0 {
            return Err(IoError::last_os_error());
        }
        Ok(())
    }

    pub fn stream<'a>(&'a mut self) -> impl Stream<Item = Result<INotifyEvent, IoError>> + 'a {
        stream! {
            let mut buf = [0u8; EVENT_SIZE + NAME_MAX + 1];
            loop {
                let read_len = self.stream.read(&mut buf[..EVENT_SIZE + NAME_MAX + 1]).await?;
                let mut buf_ref = &mut buf[..read_len];
                while !buf_ref.is_empty() {
                    if buf_ref.len() < EVENT_SIZE {
                        continue;
                    }
                    let raw_event: RawINotifyEvent = *unsafe { (buf_ref.as_ptr() as *const RawINotifyEvent).as_ref().unwrap() };
                    buf_ref = &mut buf_ref[EVENT_SIZE..];
                    if read_len - EVENT_SIZE < raw_event.len as usize {
                        continue;
                    }
                    let mut path = &buf_ref[..raw_event.len as usize];
                    while !path.is_empty() && path[path.len() - 1] == 0 {
                        path = &path[..path.len() - 1];
                    }
                    let path = OsString::from_vec(path.to_vec());
                    buf_ref = &mut buf_ref[raw_event.len as usize..];
                    yield Ok(INotifyEvent {
                        watch_descriptor: raw_event.watch_descriptor,
                        mask: raw_event.mask,
                        cookie: raw_event.cookie,
                        name: path,
                    });
                }
            }
        }
    }
}
