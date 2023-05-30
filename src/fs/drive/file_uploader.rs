use std::collections::HashMap;
use std::fmt::Debug;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use anyhow::Context;
use google_drive3::api::File;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, instrument, warn};

use crate::config::common_file_filter::CommonFileFilter;
use crate::google_drive::GoogleDrive;

#[derive(Debug, Clone)]
pub struct FileCommand {
    path: PathBuf,
    file_metadata: File,
}

impl FileCommand {
    pub fn new(path: PathBuf, file_metadata: File) -> Self {
        Self {
            path,
            file_metadata,
        }
    }
}

#[derive(Debug)]
struct RunningUpload {
    join_handle: JoinHandle<anyhow::Result<()>>,
    stop_sender: Sender<()>,
}

#[derive(Debug)]
pub enum FileUploaderCommand {
    UploadChange(FileCommand),
    CreateFolder(FileCommand),
    CreateFile(FileCommand),
    Stop,
}

#[derive(Debug)]
pub struct DriveFileUploader {
    drive: GoogleDrive,

    /// the filter to apply when uploading files
    upload_filter: CommonFileFilter,

    /// the queue of files to upload
    upload_queue: Vec<PathBuf>,
    receiver: Receiver<FileUploaderCommand>,
    wait_time_before_upload: Duration,

    running_uploads: HashMap<String, RunningUpload>,
}

impl<'a> DriveFileUploader {
    #[instrument]
    pub fn new(
        drive: GoogleDrive,
        upload_filter: CommonFileFilter,
        receiver: Receiver<FileUploaderCommand>,
        wait_time_before_upload: Duration,
    ) -> Self {
        Self {
            drive,
            upload_filter,
            upload_queue: Vec::new(),
            receiver,
            wait_time_before_upload,
            running_uploads: HashMap::new(),
        }
    }
    #[instrument(skip(self), fields(self.upload_queue = self.upload_queue.len(),
    self.upload_filter = self.upload_filter.filter.num_ignores()))]
    pub async fn listen(&mut self) {
        info!("listening for file upload requests");
        loop {
            // while let Some(command) = self.receiver.recv().await {
            let command = self.receiver.recv().await;
            if let Some(command) = command {
                debug!("received path: {:?}", command);
                debug!("received path: {:?}", command);
                match command {
                    FileUploaderCommand::UploadChange(file_command) => {
                        let path = file_command.path;
                        let file_metadata = file_command.file_metadata;
                        if !self.upload_filter.is_filter_matched(&path).unwrap_or(false) {
                            let drive = self.drive.clone();
                            let drive_id = file_metadata
                                .drive_id
                                .clone()
                                .with_context(|| "no drive_id");
                            if let Err(e) = drive_id {
                                error!("failed to upload file: {:?} with error: {}", path, e);
                                continue;
                            }
                            let drive_id = drive_id.unwrap();

                            self.cancel_and_wait_for_running_upload_for_id(&drive_id)
                                .await;

                            info!("queuing upload of file: {:?}", path);
                            let wait_time_before_upload = self.wait_time_before_upload.clone();
                            let (rx, rc) = channel(1);
                            let upload_handle = tokio::spawn(async move {
                                Self::upload_file(
                                    drive,
                                    file_metadata,
                                    path,
                                    wait_time_before_upload,
                                    rc,
                                )
                                .await
                            });
                            self.running_uploads.insert(
                                drive_id,
                                RunningUpload {
                                    join_handle: upload_handle,
                                    stop_sender: rx,
                                },
                            );
                        } else {
                            info!("skipping upload of file since it is ignored: {:?}", path);
                        }
                    }
                    FileUploaderCommand::Stop => {
                        info!("received stop command: stopping file upload listener");
                        break;
                    }
                    _ => {
                        warn!("received unknown command: {:?}", command);
                    }
                };
            } else {
                warn!(
                    "received None command, meaning all senders have been dropped. \
                stopping file upload listener since no more commands will be received"
                );
                break;
            }
        }

        info!("file upload listener stopped");
    }

    /// this function checks if there are any running uploads for the given drive_id
    /// and if there are, it sends a stop command to all of them and then awaits for them to finish
    async fn cancel_and_wait_for_running_upload_for_id(&mut self, drive_id: &String) {
        debug!("checking for running uploads for file: {:?}", drive_id);
        let running_uploads: Option<&mut RunningUpload> = self.running_uploads.get_mut(drive_id);
        if let Some(running_upload) = running_uploads {
            debug!(
                "trying to send stop command to running upload for file: {:?}",
                drive_id
            );
            let send_stop = running_upload.stop_sender.send(()).await;
            if let Err(e) = send_stop {
                error!(
                    "failed to send stop command to running upload for file: {:?} with error: {}",
                    drive_id, e
                );
            }

            debug!("waiting for running upload for file: {:?}", drive_id);
            let x: &mut JoinHandle<anyhow::Result<()>> = &mut running_upload.join_handle;
            let _join_res = tokio::join!(x);
            debug!(
                "finished waiting for running upload for file: {:?} ",
                drive_id
            );

            debug!("removing running upload for file: {:?}", drive_id);
            self.running_uploads.remove(drive_id);
        }
    }
    #[instrument(skip(file_metadata, rc), fields(drive = % drive))]
    async fn upload_file(
        drive: GoogleDrive,
        file_metadata: File,
        local_path: PathBuf,
        wait_time_before_upload: Duration,
        rc: Receiver<()>,
    ) -> anyhow::Result<()> {
        // debug!("uploading file: {:?}", local_path);
        debug!(
            "sleeping for {:?} before uploading {}",
            wait_time_before_upload,
            local_path.display()
        );
        tokio::select! {
            _ = Self::wait_for_cancel_signal(rc) => {
                debug!("received stop signal: stopping upload");
                return Ok(());
            },
            _ = tokio::time::sleep(wait_time_before_upload)=> {
                debug!("done sleeping");
                return Self::upload_file_(&drive, file_metadata, &local_path)
                    .await
                    .map_err(|e| {
                        error!("error uploading file: {:?}: {:?}", local_path, e);
                        // FileUploadError {
                        //     path: local_path,
                        //     error: anyhow!(e),
                            anyhow!(e)
                        // }
                    });
            }
        }
    }

    #[instrument(skip(rc))]
    async fn wait_for_cancel_signal(mut rc: Receiver<()>) {
        match rc.recv().await {
            Some(_v) => {
                debug!("received stop signal: stopping upload");
            }
            _ => {
                warn!("received None from cancel signal receiver")
            }
        }
    }
    async fn upload_file_(
        drive: &GoogleDrive,
        file_metadata: File,
        local_path: &PathBuf,
    ) -> anyhow::Result<()> {
        debug!("uploading file: {:?}", local_path);
        let path = local_path.as_path();
        drive
            .upload_file_content_from_path(file_metadata, path)
            .await?;
        // let result = drive.list_files(DriveId::from("root")).await.with_context(|| format!("could not do it"))?;
        debug!("upload_file_: done");

        Ok(())
    }
}
