#[tokio::main]
async fn main() {
    drive_syncer::init_logger();
    // use tokio::runtime::Runtime;

    // let rt = Runtime::new().unwrap();
    // let filesystem_runtime = Runtime::new().unwrap();
    //
    // let handle = rt.handle();
    // handle.block_on(async {
    // drive_syncer::sample().await.unwrap();
    // drive_syncer::sample_fs().await.unwrap();
    // drive_syncer::google_drive::sample().await.unwrap();
    // drive_syncer::watch_file_reading().await.unwrap();
    // drive_syncer::sample_nix().await.unwrap();
    drive_syncer::sample_drive_fs().await.unwrap();
    // });
    // RUNTIME.block_on(async {
    //     //test
    // });
}
