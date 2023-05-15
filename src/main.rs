#[tokio::main]
async fn main() {
    drive_syncer::init_logger();
    // drive_syncer::sample().await.unwrap();
    // drive_syncer::google_drive::sample().await.unwrap();
    // drive_syncer::watch_file_reading().await.unwrap();
    // drive_syncer::sample_nix().await.unwrap();

    // drive_syncer::sample_fs().await.unwrap();
    drive_syncer::sample_drive_fs().await.unwrap();
}
