#[tokio::main]
async fn main() {
    untitled::init_logger();
    // use tokio::runtime::Runtime;

    // let rt = Runtime::new().unwrap();
    // let filesystem_runtime = Runtime::new().unwrap();
    //
    // let handle = rt.handle();
    // handle.block_on(async {
    // untitled::sample().await.unwrap();
    // untitled::sample_fs().await.unwrap();
    // untitled::google_drive::sample().await.unwrap();
    // untitled::watch_file_reading().await.unwrap();
    // untitled::sample_nix().await.unwrap();
    untitled::sample_drive_fs().await.unwrap();
    // });
    // RUNTIME.block_on(async {
    //     //test
    // });
}
