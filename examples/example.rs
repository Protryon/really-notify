use really_notify::FileWatcherConfig;

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .parse_env(env_logger::Env::default().default_filter_or("info"))
        .init();
    // the general idea here by bundling the parser in, is that retry logic is embedded.
    // if the config becomes invalid (fails to parse, fails validation, etc), then the old config can be used in the meantime
    // while `really-notify` spits out errors to the log for the user.

    // if the file doesn't exist, isn't readable, can't be parsed, etc, then `really-notify` will enter a 1-second loop to reattempt and print errors.
    // this helps recover against not having read permissions, which prevents us from watching the file for changes as well.
    let mut receiver = FileWatcherConfig::new("./examples/config.yaml", "config")
        .with_parser(|data| String::from_utf8(data))
        .start();
    while let Some(config) = receiver.recv().await {
        // so, everytime we get here, we have a new valid config to throw in an `ArcSwap`/`tokio::sync::watch`/etc. No further validation needed.
        println!("got new config!\n{config}");
    }
    // when `receiver` is dropped, all of the `inotify` stuff gets cleaned up.
}
