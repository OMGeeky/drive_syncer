// #![allow(dead_code, unused)]

use fuser::{MountOption, Session, SessionUnmounter};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task::JoinHandle,
};
use tracing::{debug, error, info};

use prelude::*;

use crate::{
    config::common_file_filter::CommonFileFilter,
    fs::drive::{DriveFileUploader, DriveFilesystem, FileUploaderCommand, SyncSettings},
    fs::drive_file_provider::{ProviderCommand, ProviderRequest},
    fs::{drive2, drive_file_provider},
    google_drive::GoogleDrive,
};

pub mod async_helper;
pub mod common;
pub mod config;
pub mod fs;
pub mod google_drive;
mod macros;
pub mod prelude;

//region drive2 full example
pub async fn sample_drive2() -> Result<()> {
    let mountpoint = Path::new("/tmp/fuse/3");
    let perma_dir = Path::new("/tmp/fuse/2");
    let cache_dir = get_cache_dir()?;

    let (provider_command_tx, provider_command_rx) = channel(1);
    let (provider_request_tx, provider_request_rx) = channel(1);

    let (filesystem_handle, unmount_callable) =
        filesystem_thread_starter(provider_request_tx, mountpoint).await?;
    let provider_handle = provider_thread_starter(
        provider_command_rx,
        provider_request_rx,
        unmount_callable,
        cache_dir.path(),
        perma_dir,
    )
    .await?;

    let program_end_handle = ctrl_c_thread_starter().await?;
    select! {
        _= filesystem_handle => {
            info!("filesystem thread finished first!");
            let x = provider_command_tx.send(ProviderCommand::Stop).await;
            info!("send stop to provider: {:?}", x);
        },
        _= program_end_handle => {
            info!("filesystem thread finished first!");
            let x = provider_command_tx.send(ProviderCommand::Stop).await;
            info!("send stop to provider: {:?}", x);
        },
    }
    provider_handle.await?;
    info!("everything finished! Exiting...");
    Ok(())
}

async fn filesystem_thread_starter(
    provider_request_tx: Sender<ProviderRequest>,
    mountpoint: impl Into<&Path>,
) -> Result<(JoinHandle<()>, SessionUnmounter)> {
    let filesystem = drive2::DriveFilesystem::new(provider_request_tx);
    let mount_options = vec![
        MountOption::RW, /*TODO: make a start parameter that can change the mount to read only*/
    ];
    let mut mount = Session::new(filesystem, mountpoint.into(), &mount_options)?;
    let session_unmounter = mount.unmount_callable();
    let join_handle = tokio::spawn(async move {
        let mount_res = mount.run();
        debug!("mount finished with result: {:?}", mount_res);
        if let Err(e) = mount_res {
            error!("mount finished with error: {:?}", e);
        }
    });
    Ok((join_handle, session_unmounter))
}

async fn provider_thread_starter(
    provider_command_rx: Receiver<ProviderCommand>,
    provider_request_rx: Receiver<ProviderRequest>,
    mut unmount_callable: SessionUnmounter,
    cache_dir: &Path,
    perma_dir: &Path,
) -> Result<JoinHandle<()>> {
    let drive = GoogleDrive::new().await?;

    let changes_start_token = drive
        .get_start_page_token()
        .await
        .expect("could not initialize the changes api start page token");
    let mut provider = drive_file_provider::DriveFileProvider::new(
        drive,
        cache_dir.to_path_buf(),
        perma_dir.to_path_buf(),
        changes_start_token,
    );

    Ok(tokio::spawn(async move {
        provider
            .listen(provider_request_rx, provider_command_rx)
            .await;
        unmount_callable.unmount().expect("failed to unmount");
    }))
}
async fn ctrl_c_thread_starter() -> Result<JoinHandle<()>> {
    Ok(tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl_c event");

        info!("got signal to end program");
    }))
}
//endregion

//region old examples
pub async fn sample_drive2_fs() -> Result<()> {
    // let mountpoint = "/tmp/fuse/3";
    let mountpoint = Path::new("/tmp/fuse/3");
    let perma_dir = "/tmp/fuse/2";

    let cache_dir = get_cache_dir()?;

    let drive = GoogleDrive::new().await?;
    let test = drive.list_all_files().await;
    debug!("test!");
    for entry in test.unwrap() {
        debug!("entry: {:?}", entry);
    }
    debug!("test!");
    let (provider_tx, provider_rx) = channel(1);
    let filesystem = drive2::DriveFilesystem::new(provider_tx);
    let mount_options = vec![MountOption::RW];
    let mut mount = Session::new(filesystem, &mountpoint, &mount_options)?;
    let mut session_unmounter = mount.unmount_callable();

    let (command_tx, command_rx) = channel(1);
    let provider_join_handle: JoinHandle<()> = tokio::spawn(drive2_provider(
        drive,
        cache_dir.path().to_path_buf(),
        PathBuf::from(perma_dir),
        provider_rx,
        command_rx,
    ));
    debug!("running mount and listener");
    select!(
        _= async move {mount.run()} => {
            debug!("mount.run finished first!");
            let _ = command_tx.send(ProviderCommand::Stop);
            let _ = session_unmounter.unmount();
        },
        _=provider_join_handle => {
            debug!("provider finished first!");
            let _ = session_unmounter.unmount();
        }
    );

    Ok(())
}
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
    let (file_uploader_sender, file_uploader_receiver) = channel(1);
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
async fn drive2_provider(
    drive: GoogleDrive,
    cache_dir: PathBuf,
    perma_dir: PathBuf,
    provider_rx: Receiver<ProviderRequest>,
    command_rx: Receiver<ProviderCommand>,
) {
    let changes_start_token = drive
        .get_start_page_token()
        .await
        .expect("could not initialize the changes api start page token");
    let mut provider = drive_file_provider::DriveFileProvider::new(
        drive,
        cache_dir,
        perma_dir,
        changes_start_token,
    );
    provider.listen(provider_rx, command_rx).await;
}
//endregion

#[cfg(test)]
pub mod tests {
    pub fn init_logs() {
        use tracing::Level;
        use tracing_subscriber::fmt;
        use tracing_subscriber::EnvFilter;
        // Create a new subscriber with the default configuration
        let subscriber = fmt::Subscriber::builder()
            .with_test_writer()
            // .with_thread_ids(true)
            .with_env_filter(EnvFilter::from_default_env())
            .with_max_level(Level::DEBUG)
            .with_line_number(true)
            .with_target(true)
            .with_file(true)
            // .with_span_events(fmt::format::FmtSpan::NONE)
            .finish();

        // Install the subscriber as the default for this thread
        let _ = tracing::subscriber::set_global_default(subscriber); //.expect("setting default subscriber failed");
    }
}
