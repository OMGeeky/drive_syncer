// #![allow(dead_code, unused)]

extern crate google_drive3 as drive3;

use std::path::Path;
use std::time::Duration;

use fuser::{MountOption, Session, SessionUnmounter};
// use nix;
use tempfile::TempDir;
// use tokio::io::{AsyncReadExt, stdin};
// use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;
use tracing::{debug, info};

use prelude::*;

use crate::config::common_file_filter::CommonFileFilter;
use crate::fs::drive::{DriveFileUploader, DriveFilesystem, FileUploaderCommand, SyncSettings};
use crate::google_drive::GoogleDrive;

pub mod async_helper;
pub mod common;
pub mod config;
pub mod fs;
pub mod google_drive;
pub mod prelude;

pub async fn sample_drive_fs() -> Result<()> {
    let mountpoint = "/tmp/fuse/3";
    let upload_ignore_path = Path::new("config/.upload_ignore");
    // let settings_path = Path::new("config/settings.json");

    let cache_dir = get_cache_dir()?;
    let upload_ignore = CommonFileFilter::from_path(upload_ignore_path)?;
    let sync_settings = SyncSettings::new(Duration::from_secs(2), Duration::from_secs(5));
    // let source = "/tmp/fuse/2";
    let drive = GoogleDrive::new().await?;
    // let file_uploader = FileUploader::new("config/credentials.json", "config/token.json");
    let (file_uploader_sender, file_uploader_receiver) = mpsc::channel(1);
    let mut file_uploader = DriveFileUploader::new(
        drive.clone(),
        upload_ignore,
        file_uploader_receiver,
        Duration::from_secs(3),
    );
    debug!("Mounting fuse filesystem at {}", mountpoint);
    let fs = DriveFilesystem::new(
        Path::new(""),
        file_uploader_sender.clone(),
        drive,
        cache_dir.into_path(),
        sync_settings,
    )
    .await?;

    let mount_options = vec![MountOption::RW];

    let uploader_handle: JoinHandle<()> = tokio::spawn(async move {
        file_uploader.listen().await;
    });
    let end_signal_handle: JoinHandle<()> =
        mount(fs, &mountpoint, &mount_options, file_uploader_sender).await?;
    tokio::try_join!(uploader_handle, end_signal_handle)?;

    debug!("Exiting gracefully...");
    Ok(())
}

fn get_cache_dir() -> Result<TempDir> {
    let cache_dir = tempfile::tempdir()?;
    debug!("cache_dir: {:?}", cache_dir.path());
    if !cache_dir.path().exists() {
        debug!("creating cache dir: {:?}", cache_dir.path());
        std::fs::create_dir_all(cache_dir.path())?;
    } else {
        debug!("cache dir exists: {}", cache_dir.path().display());
    }
    Ok(cache_dir)
}

async fn mount(
    fs: DriveFilesystem,
    mountpoint: &str,
    options: &[MountOption],
    sender: Sender<FileUploaderCommand>,
) -> Result<JoinHandle<()>> {
    let mut session = Session::new(fs, mountpoint.as_ref(), options)?;
    let session_ender = session.unmount_callable();
    let end_program_signal_handle = tokio::spawn(async move {
        let _ = end_program_signal_awaiter(sender, session_ender).await;
    });
    debug!("Mounting fuse filesystem");
    let _ = session.run();
    debug!("Stopped with mounting");
    // Ok(session_ender)
    Ok(end_program_signal_handle)
}

async fn end_program_signal_awaiter(
    file_uploader_sender: Sender<FileUploaderCommand>,
    mut session_unmounter: SessionUnmounter,
) -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl_c event");

    info!("got signal to end program");
    file_uploader_sender.send(FileUploaderCommand::Stop).await?;
    info!("sent stop command to file uploader");
    info!("unmounting...");
    session_unmounter.unmount()?;
    info!("unmounted");
    Ok(())
}
