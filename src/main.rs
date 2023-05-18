use tokio::io::AsyncReadExt;
use tracing::instrument::WithSubscriber;
use tracing::span;

#[tokio::main]
async fn main() {
    // drive_syncer::init_logger();
    init_tracing();
    // drive_syncer::sample().await.unwrap();
    // drive_syncer::google_drive::sample().await.unwrap();
    // drive_syncer::watch_file_reading().await.unwrap();
    // drive_syncer::sample_nix().await.unwrap();

    // drive_syncer::sample_fs().await.unwrap();

    sample_logging().await;
    drive_syncer::sample_drive_fs().await.unwrap();
}

fn init_tracing() {
    // use tracing::Level;
    // use tracing_subscriber::fmt;
    // use tracing_subscriber::EnvFilter;
    // // Create a new subscriber with the default configuration
    // let subscriber = fmt::Subscriber::builder()
    //
    //     // .with_thread_ids(true)
    //     .with_env_filter(EnvFilter::from_default_env())
    //     .with_max_level(Level::DEBUG)
    //     .with_line_number(true)
    //     .with_target(true)
    //     .with_file(true)
    //     // .with_span_events(fmt::format::FmtSpan::NONE)
    //     .finish();
    //
    // // Install the subscriber as the default for this thread
    // tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    console_subscriber::init();
    tracing::info!("tracing initialized");
}

#[tracing::instrument]
async fn sample_logging() {
    use tracing::{debug, error, info, trace, warn};
    info!("info");
    debug!("debug");
    let s = span!(tracing::Level::TRACE, "span around trace and warn with stdin read");
    {
        let _x = s.enter();
        trace!("trace");
        let mut string = [0u8; 1];
        // info!("press any key to continue");
        // tokio::io::stdin().read(&mut string).await.expect("failed to read stdin");
        warn!("warn");
    }
    error!("error");
}
