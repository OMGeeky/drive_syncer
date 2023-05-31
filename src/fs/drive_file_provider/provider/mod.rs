use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::{Debug, Formatter};
use std::fs::Permissions;
use std::io::SeekFrom;
use std::os::unix::prelude::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Error};
use fuser::{FileAttr, FileType};
use libc::c_int;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::{fs, join};
use tracing::{debug, error, info, instrument, trace, warn};

use crate::fs::drive2::HandleFlags;
use crate::fs::drive_file_provider::{
    FileMetadata, ProviderLookupRequest, ProviderMetadataRequest, ProviderOpenFileRequest,
    ProviderReadContentRequest, ProviderReadDirRequest, ProviderReadDirResponse,
    ProviderReleaseFileRequest, ProviderRequest, ProviderRequestStruct, ProviderResponse,
    ProviderSetAttrRequest, ProviderWriteContentRequest,
};
use crate::google_drive::{DriveId, GoogleDrive};
use crate::prelude::*;
use crate::{send_error_response, send_response};

#[derive(Debug)]
pub enum ProviderCommand {
    Stop,
    PauseSync,
}
#[derive(Debug)]
pub struct FileRequest {
    pub file_id: DriveId,
    pub response_sender: Sender<FileData>,
}

#[derive(Debug, Clone)]
pub struct FileData {
    // pub local_path: PathBuf,
    pub metadata: DriveFileMetadata,
    pub changed_metadata: DriveFileMetadata,
    /// marks if a file should be kept up to date locally, even without internet connection
    /// the file is accessible.
    ///
    /// This can lead to conflicts of being locally edited and remote!
    pub perma: bool,
    pub attr: FileAttr,
    pub is_local: bool,
}

#[derive(Debug)]
pub struct FileHandleData {
    flags: HandleFlags,
    file: Option<File>,
    path: PathBuf,
    creating: bool,
    marked_for_open: bool,
    has_content_changed: bool,
}

pub struct DriveFileProvider {
    drive: GoogleDrive,
    cache_dir: PathBuf,
    perma_dir: PathBuf,

    // file_request_receiver: std::sync::mpsc::Receiver<ProviderRequest>,
    running_requests: HashMap<DriveId, JoinHandle<Result<()>>>,
    alt_root_id: DriveId,
    entries: HashMap<DriveId, FileData>,
    parents: HashMap<DriveId, Vec<DriveId>>,
    children: HashMap<DriveId, Vec<DriveId>>,

    file_handles: HashMap<u64, FileHandleData>,
    next_fh: u64,
}
impl Debug for DriveFileProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveFileProvider")
            // .field("running_requests", &self.running_requests.len())
            .field("entries", &self.entries.len())
            .field("children", &self.children.len())
            .field("parents", &self.parents.len())
            .field("file_handles", &self.file_handles.len())
            .field("next_fh", &self.next_fh)
            // .field("cache_dir", &self.cache_dir)
            // .field("perma_dir", &self.perma_dir)
            .finish()
    }
}
impl DriveFileProvider {
    pub fn new(
        drive: GoogleDrive,
        cache_dir: PathBuf,
        perma_dir: PathBuf,
        // file_request_receiver: std::sync::mpsc::Receiver<ProviderRequest>,
    ) -> Self {
        Self {
            drive,
            cache_dir,
            perma_dir,
            // file_request_receiver,
            running_requests: HashMap::new(),
            alt_root_id: DriveId::root(),
            entries: HashMap::new(),
            parents: HashMap::new(),
            children: HashMap::new(),
            file_handles: HashMap::new(),
            next_fh: 111,
        }
    }
    fn add_parent_child_relation(&mut self, parent_id: DriveId, child_id: DriveId) {
        trace!(
            "adding child-parent relation for child: {:<50} and parent: {:<50}",
            child_id,
            parent_id
        );
        if let Some(parents) = self.parents.get_mut(&child_id) {
            parents.push(parent_id.clone());
        } else {
            self.parents
                .insert(child_id.clone(), vec![parent_id.clone()]);
        }
        if let Some(children) = self.children.get_mut(&parent_id) {
            children.push(child_id);
        } else {
            self.children.insert(parent_id, vec![child_id]);
        }
    }
    #[instrument(skip(self, request_reciever, command_receiver))]
    pub async fn listen(
        &mut self,
        request_reciever: Receiver<ProviderRequest>,
        command_receiver: Receiver<ProviderCommand>,
    ) {
        debug!("listen");
        tokio::select! {
            _ = Self::listen_for_stop(command_receiver) => {
                trace!("DriveFileProvider::listen_for_stop() finished");
                self.cleanup().await;
            },
            _ = self.listen_for_file_requests(request_reciever) => {trace!("DriveFileProvider::listen_for_file_requests() finished");},
        }
    }
    pub async fn listen_for_stop(mut command_receiver: Receiver<ProviderCommand>) {
        let signal = command_receiver.recv().await;
        if let Some(signal) = signal {
            match signal {
                ProviderCommand::Stop => {
                    debug!("provider received stop command");
                }
                _ => {
                    error!("unknown signal");
                    todo!()
                }
            }
        }
        // sleep(std::time::Duration::from_secs(
        //     10 * 60 * 60 * 24, /*10 days*/
        // ))
        // .await;
        debug!("listen for stop finished");
        // //TODO: implement waiting for the stop signal instead of just waiting for 10 days
    }
    pub async fn cleanup(&mut self) {
        debug!("cleanup got called");
        todo!("cleanup")
    }
    #[instrument(skip(self, rx))]
    pub async fn listen_for_file_requests(
        &mut self,
        rx: tokio::sync::mpsc::Receiver<ProviderRequest>,
    ) {
        debug!("initializing entries");
        let init_res = self.initialize_entries().await;
        if let Err(e) = init_res {
            error!("got an error at initialize_entries: {}", e);
            todo!("maybe implement error handling for this (or just leave it, idc)")
        }
        debug!("listening for file requests");
        let mut rx = rx;
        while let Some(file_request) = rx.recv().await {
            debug!("got file request: {:?}", file_request);
            let result = match file_request {
                ProviderRequest::OpenFile(r) => self.open_file(r).await,
                ProviderRequest::ReleaseFile(r) => self.release_file(r).await,
                ProviderRequest::Metadata(r) => self.metadata(r).await,
                ProviderRequest::ReadContent(r) => self.read_content(r).await,
                ProviderRequest::WriteContent(r) => self.write_content(r).await,
                ProviderRequest::ReadDir(r) => self.read_dir(r).await,
                ProviderRequest::Lookup(r) => self.lookup(r).await,
                ProviderRequest::SetAttr(r) => self.set_attr(r).await,
                _ => {
                    error!("DriveFileProvider::listen_for_file_requests() received unknown request: {:?}", file_request);
                    todo!("handle this unknown request")
                }
            };
            if let Err(e) = result {
                error!("file request handler returned an error: {}", e);
            }
            debug!("processed file request, waiting for more...");
        }
        debug!("Received None from file request receiver, that means all senders have been dropped. Ending listener");
    }

    //region request handlers
    //region lookup
    #[instrument(skip(request))]
    async fn lookup(&self, request: ProviderLookupRequest) -> Result<()> {
        let name = request.name.into_string();
        if name.is_err() {
            return send_error_response!(request, anyhow!("invalid name"), libc::EINVAL);
        }
        let name = name.unwrap();
        let parent_id = self.get_correct_id(request.parent);
        debug!("looking up {} under id {}", name, parent_id);
        let children = self.children.get(&parent_id);

        // let mut result = vec![];
        for child in children.unwrap_or(&vec![]) {
            if let Some(child) = self.entries.get(child) {
                if child
                    .metadata
                    .name
                    .as_ref()
                    .unwrap_or(&"NO_NAME".to_string())
                    .eq_ignore_ascii_case(&name)
                {
                    // let response = result.push(Self::create_file_metadata_from_entry(child));
                    let result = Self::create_file_metadata_from_entry(child);
                    let response = ProviderResponse::Lookup(Some(result));
                    return send_response!(request, response);
                }
            }
        }
        info!("could not find file: {} in {}", name, parent_id);
        let response = ProviderResponse::Lookup(None);
        return send_response!(request, response);
    }
    //endregion
    //region read dir
    #[instrument(skip(request))]
    async fn read_dir(&mut self, request: ProviderReadDirRequest) -> Result<()> {
        let parent_id = self.get_correct_id(request.file_id.clone());
        debug!(
            "got read dir request for id: {} with offset: {}",
            parent_id, request.offset
        );
        if let Some(children) = self.children.get(&parent_id) {
            let response = children
                .iter()
                .map(|id| (id, self.entries.get(id)))
                .filter(|(_id, e)| e.is_some())
                .map(|(id, e)| (id, e.unwrap()))
                .map(|(id, e)| FileMetadata {
                    id: id.clone(),
                    name: e
                        .metadata
                        .name
                        .as_ref()
                        .unwrap_or(&"NO_NAME".to_string())
                        .clone(),
                    attr: e.attr.clone(),
                })
                .skip(request.offset as usize)
                .collect::<Vec<FileMetadata>>();
            debug!("returning {} entries", response.len());
            let response = ProviderReadDirResponse { entries: response };
            return send_response!(request, ProviderResponse::ReadDir(response));
        }
        debug!("found no entries to return");
        for e in self.entries.iter() {
            debug!("entry: {}: {:?}", e.0, e.1);
            debug!("children: {:?}", self.children.get(e.0));
            debug!("parents: {:?}", self.parents.get(e.0));
        }
        return send_response!(
            request,
            ProviderResponse::ReadDir(ProviderReadDirResponse { entries: vec![] })
        );
    }
    //endregion
    //region open file
    #[instrument(skip(request))]
    async fn open_file(&mut self, request: ProviderOpenFileRequest) -> Result<()> {
        let file_id = &self.get_correct_id(request.file_id.clone());
        let wait_res = self
            .wait_for_running_drive_request_if_exists(&file_id)
            .await;
        if let Err(e) = wait_res {
            return send_error_response!(request, e, libc::EIO);
        }
        let target_path = self.construct_path(&file_id);
        if let Err(e) = target_path {
            return send_error_response!(request, e, libc::EIO);
        }
        let target_path = target_path.unwrap();
        if !self
            .entries
            .get(file_id)
            .map(|e| e.is_local)
            .unwrap_or(false)
        {
            debug!("file not local, downloading...");
            let drive = self.drive.clone();
            self.start_download_call(&request, drive, &target_path)
                .await?;
        }
        let handle_flags = HandleFlags::from(request.flags);
        let fh = self.create_fh(handle_flags, target_path, false, true);
        send_response!(request, ProviderResponse::OpenFile(fh, handle_flags))
    }
    //endregion
    //region release file
    #[instrument(skip(request))]
    async fn release_file(&mut self, request: ProviderReleaseFileRequest) -> Result<()> {
        let file_id = &self.get_correct_id(request.file_id.clone());
        let wait_res = self
            .wait_for_running_drive_request_if_exists(&file_id)
            .await;
        if let Err(e) = wait_res {
            return send_error_response!(request, e, libc::EIO);
        }
        let entry = self.entries.get(file_id).context("could not get entry");
        if let Err(e) = entry {
            return send_error_response!(request, e, libc::EIO);
        }
        let entry = entry.unwrap();
        let file_handle = self
            .file_handles
            .remove(&request.fh)
            .context("could not get entry");
        if let Err(e) = file_handle {
            return send_error_response!(request, e, libc::EIO);
        }
        let file_handle = file_handle.unwrap();
        if file_handle.has_content_changed {
            debug!("uploading changes to google drive for file: {}", file_id);
            let drive = self.drive.clone();
            let start_result = self.start_upload_call(file_id.clone(), drive).await;
            if let Err(e) = start_result {
                error!("got error from starting the upload: {:?}", e);
                return send_error_response!(request, e, libc::EIO);
            }
        }
        return send_response!(request, ProviderResponse::ReleaseFile);
    }
    //endregion
    //region metadata
    #[instrument(skip(request))]
    async fn metadata(&self, request: ProviderMetadataRequest) -> Result<()> {
        let file_id = &self.get_correct_id(request.file_id.clone());
        debug!("metadata got called");
        let entry = self.entries.get(file_id);
        if entry.is_none() {
            return send_error_response!(
                request,
                anyhow!("could not find entry with id"),
                libc::ENOENT
            );
        }
        let entry = entry.unwrap();
        let response = ProviderResponse::Metadata(Self::create_file_metadata_from_entry(entry));

        send_response!(request, response)
    }

    //endregion
    //region set_attr
    async fn set_attr(&mut self, request: ProviderSetAttrRequest) -> Result<()> {
        let file_id = &self.get_correct_id(request.file_id.clone());
        let wait_res = self
            .wait_for_running_drive_request_if_exists(&file_id)
            .await;
        if let Err(e) = wait_res {
            return send_error_response!(request, e, libc::EIO);
        }
        debug!("set_attr got called");
        let entry = self.entries.get(file_id);
        if entry.is_none() {
            return send_error_response!(
                request,
                anyhow!("could not find entry with id"),
                libc::ENOENT
            );
        }
        let entry = entry.unwrap();
        let mut attr = entry.attr.clone();

        if let Some(size) = request.size {
            attr.size = size;
            let x = self
                .set_underlying_file_size(&file_id, request.fh, size)
                .await;
            if let Err(e) = x {
                error!(
                    "got an error while setting the underlying file size: {:?}",
                    e
                );
                return send_error_response!(request, e, libc::EIO);
            }
        }
        if let Some(flags) = request.flags {
            attr.flags = flags;
        }
        if let Some(mode) = request.mode {
            //TODO2: check if setting attr.perm to mode in setattr is correct (probably)
            // and if i can just cast it to u16 (from u32) (i have no Idea)
            attr.perm = mode as u16;
            // TODO3: check if the file below even needs me to set the permissions
            //  on the underlying file or if this is not needed at all since
            //  permissions don't get transferred to gdrive and locally i
            //  have the info in the entries

            // if let Some(fh) = request.fh {
            //     let handle = self.file_handles.get_mut(&fh);
            //     if let Some(handle) = handle {
            //         if let Some(file) = &mut handle.file {
            //             let perms = Permissions::from_mode(mode);
            //             let x = file.set_permissions(perms).await;
            //             if x.is_err() {
            //                 warn!("got an error result while setting len of file: {:?}", x);
            //             }
            //         }
            //     }
            // }
        }
        // if let Some(fh) = request.fh {
        //     //TODO2: implement something for fh in setattr
        //     warn!(
        //         "fh was set in setattr but I don't know what to do with this: {:?}",
        //         request
        //     );
        // }
        if let Some(gid) = request.gid {
            //TODO2: implement something for gid in setattr
            warn!(
                "gid was set in setattr but I don't know what to do with this: {:?}",
                request
            );
        }
        if let Some(uid) = request.uid {
            //TODO2: implement something for uid in setattr
            warn!(
                "uid was set in setattr but I don't know what to do with this: {:?}",
                request
            );
        }

        let entry = self
            .entries
            .get_mut(file_id)
            .expect("got it in here before");
        entry.attr = attr;

        let response = ProviderResponse::SetAttr(Self::create_file_metadata_from_entry(entry));

        send_response!(request, response)
    }

    async fn set_underlying_file_size(
        &mut self,
        file_id: &&DriveId,
        fh: Option<u64>,
        size: u64,
    ) -> Result<()> {
        let mut was_applied = false;
        if let Some(fh) = fh {
            let handle = self.file_handles.get_mut(&fh);
            if let Some(handle) = handle {
                if let Some(file) = &mut handle.file {
                    let x = file
                        .set_len(size)
                        .await
                        .context("could not set the len of the file");
                    if x.is_err() {
                        warn!("got an error result while setting len of file: {:?}", x);
                    } else {
                        was_applied = true;
                    }
                }
            }
        }
        if !was_applied {
            let target_path = self.construct_path(&file_id)?;
            fs::OpenOptions::new()
                .write(true)
                .open(target_path)
                .await
                .context("could not open file to set the size")?
                .set_len(size)
                .await
                .context("could not set the size of the file")?;
        }
        Ok(())
    }
    //endregion
    //region read content
    #[instrument(skip(request))]
    async fn read_content(&mut self, request: ProviderReadContentRequest) -> Result<()> {
        let file_id = &self.get_correct_id(request.file_id.clone());
        let wait_res = self
            .wait_for_running_drive_request_if_exists(&file_id)
            .await;
        if let Err(e) = wait_res {
            return send_error_response!(request, e, libc::EIO);
        }

        let data = self.read_content_from_file(&request).await;
        if let Err(e) = data {
            return send_error_response!(request, e, libc::EIO);
        }
        let data = data.unwrap();
        send_response!(request, ProviderResponse::ReadContent(data))
    }
    //endregion
    //region write content
    #[instrument(skip(request))]
    async fn write_content(&mut self, request: ProviderWriteContentRequest) -> Result<()> {
        let file_id = &self.get_correct_id(request.file_id.clone());
        let wait_res = self.wait_for_running_drive_request_if_exists(file_id).await;
        if let Err(e) = wait_res {
            return send_error_response!(request, e, libc::EIO);
        }

        let size_written = self
            .write_content_from_file(file_id.clone(), &request)
            .await;
        if let Err(e) = size_written {
            return send_error_response!(request, e, libc::EIO);
        }
        let size_written = size_written.unwrap();
        return send_response!(request, ProviderResponse::WriteSize(size_written));
    }
    //endregion

    //endregion
    //region request helpers

    /// gets the file-handle and opens the file if it is marked for open.
    ///
    /// If it is not marked for open but the file is None this returns an error
    #[instrument]
    async fn get_and_open_file_handle(&mut self, fh: u64) -> Result<&mut FileHandleData> {
        let file_handle = self.file_handles.get_mut(&fh);
        if file_handle.is_none() {
            error!("Failed to find file_handle for fh: {}", fh);
            return Err(anyhow!("Failed to find file_handle for fh: {}", fh));
        }
        let file_handle = file_handle.unwrap();
        if file_handle.file.is_none() {
            debug!("file is none, opening...");
            let flags = file_handle.flags;
            let opened_file = OpenOptions::new()
                .write(flags.can_write())
                .read(flags.can_read())
                .open(&file_handle.path)
                .await;
            if let Err(e) = &opened_file {
                let e = anyhow!("error opening the file{}", e);
                error!("{}", e);
                return Err(e);
            }
            let opened_file = opened_file.unwrap();
            file_handle.file = Some(opened_file);
            file_handle.marked_for_open = false;
        } else {
            error!("File handle does not have a file");
            return Err(anyhow!("File handle does not have a file"));
        }
        Ok(file_handle)
    }

    async fn write_content_from_file(
        &mut self,
        file_id: DriveId,
        request: &ProviderWriteContentRequest,
    ) -> Result<u32> {
        let file_handle = self.get_and_open_file_handle(request.fh).await?;
        let file = file_handle.file.as_mut().unwrap();
        if !file_handle.flags.can_write() {
            error!("File handle does not have read permissions");
            return Err(anyhow!("File handle does not have read permissions"));
        }
        debug!(
            "writing to file at local path: {}",
            file_handle.path.display()
        );
        let file: &mut tokio::fs::File = file;
        trace!("seeking position: {}", request.offset);
        file.seek(SeekFrom::Start(request.offset)).await?;
        trace!("writing data: {:?}", request.data);
        let m = file.metadata().await.unwrap();
        debug!(
            "metadata before write: size: {}; modified: {:?}",
            m.size(),
            m.modified()
        );
        let size_written = file.write(&request.data).await?;
        file.sync_all().await?;
        let m = file.metadata().await.unwrap();
        debug!(
            "metadata after  write: size: {}; modified: {:?}",
            m.size(),
            m.modified()
        );
        trace!("wrote data: size: {}", size_written);
        file_handle.has_content_changed = true;
        let entry = self.entries.get_mut(&file_id);
        if entry.is_none() {
            error!("could not find entry");
            return Err(anyhow!("could not find entry to update metadata on"));
        }
        let entry = entry.unwrap();
        let now = SystemTime::now();
        entry.attr.size += size_written as u64;
        entry.attr.atime = now;
        entry.attr.mtime = now;

        Ok(size_written as u32)
    }

    async fn read_content_from_file(
        &mut self,
        request: &ProviderReadContentRequest,
    ) -> Result<Vec<u8>> {
        let file_handle = self.get_and_open_file_handle(request.fh).await?;
        let file = file_handle.file.as_mut().expect("we just opened this...");
        if !file_handle.flags.can_read() {
            error!("File handle does not have read permissions");
            return Err(anyhow!("File handle does not have read permissions"));
        }
        trace!("seeking position in file: {}", request.offset);
        file.seek(SeekFrom::Start(request.offset)).await?;
        let mut buf = vec![0; request.size as usize];
        trace!("reading to buffer: size: {}", request.size);
        let size_read = file.read(&mut buf).await?;
        if size_read != request.size {
            warn!(
                "did not read the targeted size: target size: {}, actual size: {}",
                request.size, size_read
            );
        }
        Ok(buf)
    }
    fn create_file_metadata_from_entry(entry: &FileData) -> FileMetadata {
        FileMetadata {
            attr: entry.attr.clone(),
            name: entry
                .changed_metadata
                .name
                .as_ref()
                .unwrap_or(
                    entry
                        .metadata
                        .name
                        .as_ref()
                        .unwrap_or(&"NO_NAME".to_string()),
                )
                .clone(),
            id: DriveId::from(entry.metadata.id.as_ref().unwrap()),
        }
    }
    //endregion
    // region send response
    //
    // async fn send_response(
    //     request: &dyn ProviderRequestStruct,
    //     response_data: ProviderResponse,
    // ) -> Result<()> {
    //     let result_send_response = request.get_response_sender().send(response_data).;
    //
    //     if let Err(e) = result_send_response {
    //         error!("Failed to send result response: {:?}", e);
    //         return Err(anyhow!("Failed to send result response: {:?}", e));
    //     }
    //     Ok(())
    // }
    //
    // macro_rules! reply_error_e {
    // ($result_in:ident, $reply:ident, $error_code:expr, $error_msg:expr) => {
    //     reply_error_e!($result_in, $reply, $error_code, $error_msg,);
    // };
    // async fn send_error_response(
    //     request: &dyn ProviderRequestStruct,
    //     e: Error,
    //     code: c_int,
    // ) -> Result<()> {
    //     let error_send_response = request
    //         .get_response_sender()
    //         .send(ProviderResponse::Error(e, code));
    //     if let Err(e) = error_send_response {
    //         error!("Failed to send error response: {:?}", e);
    //         return Err(anyhow!("Failed to send error response: {:?}", e));
    //     }
    //     Ok(())
    // }
    //endregion
    //region drive helpers

    /// starts a download of the specified file and puts it in the running_requests map
    ///
    /// - will return an Error if another request is already running for the same id, so all callers should make sure of that
    async fn start_download_call(
        &mut self,
        request: &ProviderOpenFileRequest,
        drive: GoogleDrive,
        target_path: &PathBuf,
    ) -> Result<()> {
        let file_id = self.get_correct_id(request.file_id.clone());
        let id = file_id.clone();
        let entry = self.entries.get_mut(&id).context("could not find entry")?;
        entry.is_local = true;

        if let Some(_handle) = self.running_requests.get(&id) {
            return send_error_response!(
                request,
                anyhow!("Id already has a request running"),
                libc::EIO,
            );
        }
        let target_path = target_path.clone();
        let handle: JoinHandle<Result<()>> = tokio::spawn(async move {
            let _metadata: DriveFileMetadata = drive.download_file(file_id, &target_path).await?;
            Ok(())
        });

        self.running_requests.insert(id, handle);
        Ok(())
    }

    /// - will return an Error if another request is already running for the same id, so all callers should make sure of that
    async fn start_upload_call(&mut self, id: DriveId, drive: GoogleDrive) -> Result<()> {
        if self.running_requests.contains_key(&id) {
            return Err(anyhow!("Id already has a request running"));
        }

        let file_data = self
            .entries
            .get(&id)
            .context("could not find data for id")?;
        let mut metadata = file_data.changed_metadata.clone();

        let target_path = self.construct_path(&id)?;
        debug!(
            "starting upload in the background for path: '{}' and metadata: {:?}",
            target_path.display(),
            metadata
        );
        metadata.id = Some(id.clone().into());
        metadata.mime_type = file_data.metadata.mime_type.clone();
        let metadata = remove_volatile_metadata(metadata);
        let handle: JoinHandle<Result<()>> = tokio::spawn(async move {
            //TODO1: only send the changed metadata over (+id), not all of it (currently only all data that could change and where changes should be written to the drive), since google drive only wants the changes
            drive
                .upload_file_content_from_path(metadata, &target_path)
                .await?;
            Ok(())
        });
        self.running_requests.insert(id, handle);
        Ok(())
    }

    /// Checks if a drive request for this ID is running and if there is, waits for it.
    ///
    /// After awaiting, it removes the request from the map
    async fn wait_for_running_drive_request_if_exists(&mut self, file_id: &DriveId) -> Result<()> {
        if let Some(handle) = self.running_requests.get_mut(&file_id) {
            debug!("DriveFileProvider::open_file() waiting for download/upload to finish");
            let handle_result = handle.await?;
            if let Err(e) = handle_result {
                error!("async request had an error: {:?}", e);
            }
            self.running_requests.remove(&file_id);
        }
        Ok(())
    }
    //endregion

    fn create_fh(
        &mut self,
        flags: HandleFlags,
        path: PathBuf,
        create: bool,
        mark_for_open: bool,
    ) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        let file_handle = FileHandleData {
            creating: create,
            flags,
            file: None,
            path,
            marked_for_open: mark_for_open,
            has_content_changed: false,
        };
        self.file_handles.insert(fh, file_handle);
        fh
    }
    /// constructs the path where the file is stored locally. This is not necessarily a
    /// path with the correct file ending or folder structure, it could just be a unique id.
    fn construct_path(&self, id: &DriveId) -> Result<PathBuf> {
        let metadata = self.entries.get(id).context("No data found for id")?;
        //TODO: check if every drive_id is actually a valid filepath/does
        //      not contain characters that cannot be used in a path
        if metadata.perma {
            Ok(self.perma_dir.join(id.as_str()))
        } else {
            Ok(self.cache_dir.join(id.as_str()))
        }
    }
    async fn initialize_entries(&mut self) -> Result<()> {
        self.add_root_entry()
            .await
            .expect("adding the root entry has to work, otherwise nothing else works");
        let entries = self.drive.list_all_files().await?;
        for entry in entries {
            let id = &entry.id;
            if let Some(id) = id {
                let id = DriveId::from(id);
                let attr = self.create_file_attr_from_metadata(&entry);
                if attr.is_err() {
                    error!(
                        "error while creating FileAttr from metadata: {:?} entry: {:?}",
                        attr, entry
                    );
                    continue;
                }
                let attr = attr.unwrap();
                if let Some(parents) = &entry.parents {
                    for parent in parents {
                        let parent_id = DriveId::from(parent);
                        self.add_parent_child_relation(parent_id, id.clone());
                    }
                } else {
                    //file is at root level
                    self.add_parent_child_relation(
                        self.get_correct_id(DriveId::root()),
                        id.clone(),
                    );
                }
                let entry_data = FileData {
                    metadata: entry,
                    changed_metadata: Default::default(),
                    perma: false, //TODO: read the perma marker from somewhere (maybe only after all files have been checked?)
                    attr,
                    is_local: false,
                };
                self.entries.insert(id, entry_data);
            }
        }
        for (i, (id, data)) in self.entries.iter().enumerate() {
            println!("entry {:3} id: {:>40} data: {:?}", i, id, data);
        }
        Ok(())
    }
    fn create_file_attr_from_metadata(&self, metadata: &DriveFileMetadata) -> Result<FileAttr> {
        let kind = convert_mime_type_to_file_type(
            metadata.mime_type.as_ref().unwrap_or(&"NONE".to_string()),
        )?;
        // let permissions= todo!("read default permissions from a file or read specific permissions for id from somewhere (if the permissions were set in a previous sessions and stuff like that should be carried over to the next session");
        let permissions = match kind {
            FileType::Directory => 0o755,
            _ => 0o644,
        };
        let attributes = FileAttr {
            ino: 0,
            size: (*metadata.size.as_ref().unwrap_or(&0)) as u64,
            blocks: 0,
            atime: metadata
                .viewed_by_me_time
                .map(SystemTime::from)
                .unwrap_or(UNIX_EPOCH),
            mtime: metadata
                .modified_time
                .map(SystemTime::from)
                .unwrap_or(UNIX_EPOCH),
            ctime: SystemTime::now(),
            crtime: metadata
                .created_time
                .map(SystemTime::from)
                .unwrap_or(UNIX_EPOCH),
            kind,
            perm: permissions,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        };
        Ok(attributes)
    }
    async fn add_root_entry(&mut self) -> Result<()> {
        let metadata = self
            .drive
            .get_metadata_for_file(self.get_correct_id(DriveId::root()))
            .await?;
        let attr = self.create_file_attr_from_metadata(&metadata)?;
        let returned_id = metadata.id.as_ref().unwrap().clone();
        let data = FileData {
            metadata,
            changed_metadata: Default::default(),
            attr,
            perma: false,
            is_local: false,
        };

        let root_id = DriveId::from(returned_id);
        self.alt_root_id = root_id.clone();
        self.entries.insert(root_id, data);
        Ok(())
    }
    fn get_correct_id(&self, id: DriveId) -> DriveId {
        if id == DriveId::root() {
            return self.alt_root_id.clone();
        }
        return id;
    }
}

fn remove_volatile_metadata(metadata: DriveFileMetadata) -> DriveFileMetadata {
    let mut metadata = metadata;
    metadata.size = None;
    metadata.created_time = None;
    metadata.trashed_time = None;
    metadata.trashed = None;
    metadata.modified_by_me_time = None;
    metadata.modified_time = None;
    metadata.shared_with_me_time = None;
    metadata.viewed_by_me_time = None;
    metadata.explicitly_trashed = None;
    metadata.md5_checksum = None;
    metadata.parents = None;
    // parents have to be set differently: "The parents field is not directly writable in update requests. Use the addParents and removeParents parameters instead."
    metadata.kind = None;

    metadata
}

fn convert_mime_type_to_file_type(mime_type: &str) -> Result<FileType> {
    Ok(match mime_type {
        "application/vnd.google-apps.folder" => FileType::Directory,
        "application/vnd.google-apps.document"
        | "application/vnd.google-apps.spreadsheet"
        | "application/vnd.google-apps.drawing"
        | "application/vnd.google-apps.form"
        | "application/vnd.google-apps.presentation"
        | "application/vnd.google-apps.drive-sdk"
        | "application/vnd.google-apps.script"
        | "application/vnd.google-apps.*"
        //TODO: add all relevant mime types to ignore or match only the start or something
        => return Err(anyhow!("google app files are not supported (docs, sheets, etc)")),
        _ => FileType::RegularFile,
    })
}

// TODOs:
// TODO: actually upload the changes to google drive at release (start it in there, don't wait for it to finish)
// TODO: implement the changes api again (maybe with periodic updates?)
// TODO: decide what to do with a fsync
// TODO: create a way to write to a file and read
//      - read and write at least kind of work ('echo "hi" >> file' does work, opening editors like vim, nano or gui editors like kate dont, they hang up at write, open or just don't write something correct)
//          probably truncate flags or something
//           - when running 'echo "1231234" > file' first a setattr gets called, setting the size to 0, and then stuff gets written
// TODO: conform to the flags passed with open like 'read-write' or 'readonly'
