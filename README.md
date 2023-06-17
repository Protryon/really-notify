
# really-notify

This crate is for when you really, really just want to know that your config changed. K8s configmap symlink shenanigans? No problem. Multi-level symlink directory & file redirections? Sure. Just tell me my damn config changed.

## Inspiration

This crate is a derivation of some code I've been recycling for a while to deal with hot reloading K8s ConfigMaps, which relink the parent dir (accessed through a symlink). I've made it a bit more robust. This is primarily intended for use on Linux/Unix systems, however I've added a backup fallback to `notify` crate. That won't have the great symlink management the native `inotify` integration has. `notify` crate is unable to be configured to deal with symlinks properly.

Similarly, no existing inotify crate (I could find at a cursory glance) had proper async support. They all delegated out to a blocking thread at best, similar to how Tokio deals with files. To integrate with the Tokio network stack, I'm treating the `inotify` FD as a UNIX pipe receiver, which makes the correct file `read` syscall, but uses `epoll` through `mio`, and not some blocking stuff. Confirmed with `strace`.

## Examples

See `examples/` subdirectory.