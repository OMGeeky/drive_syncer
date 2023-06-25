use std::{
    collections::HashMap,
    fmt::{Debug, Formatter},
    io::SeekFrom,
    os::unix::prelude::MetadataExt,
    path::PathBuf,
    result::Result as StdResult,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context};
use fuser::{FileAttr, FileType};
use google_drive3::api::StartPageToken;
use libc::c_int;
use tokio::{
    fs,
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use tracing::{debug, error, instrument, trace, warn};

use crate::{
    common::VecExtension,
    fs::drive::{Change, ChangeType},
    fs::drive2::HandleFlags,
    fs::drive_file_provider::ProviderRenameRequest,
    fs::drive_file_provider::{
        FileMetadata, ProviderLookupRequest, ProviderMetadataRequest, ProviderOpenFileRequest,
        ProviderReadContentRequest, ProviderReadDirRequest, ProviderReadDirResponse,
        ProviderReleaseFileRequest, ProviderRequest, ProviderResponse, ProviderSetAttrRequest,
        ProviderWriteContentRequest,
    },
    google_drive::{DriveId, GoogleDrive},
    prelude::*,
    send_error_response, send_response,
};

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
impl FileData {
    fn get_id(&self) -> Option<DriveId> {
        self.metadata.id.map(|x| DriveId::from(x))
    }
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

    changes_start_token: StartPageToken,
    last_checked_for_changes: SystemTime,
    allowed_cache_time: Duration,
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
        changes_start_token: StartPageToken,
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

            changes_start_token,
            last_checked_for_changes: SystemTime::UNIX_EPOCH,
            allowed_cache_time: Duration::from_secs(10),
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

    fn remove_parent_child_relation(&mut self, parent_id: DriveId, child_id: DriveId) {
        trace!(
            "removing child-parent relation for child: {:<50} and parent: {:<50}",
            child_id,
            parent_id
        );
        if let Some(parents) = self.parents.get_mut(&child_id) {
            parents.remove_first_element(&parent_id);
        }
        if let Some(children) = self.children.get_mut(&parent_id) {
            children.remove_first_element(&child_id);
        }
    }

    //region listeners
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
            },
            _ = self.listen_for_file_requests(request_reciever) => {
                trace!("DriveFileProvider::listen_for_file_requests() finished");
            },
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
    #[instrument(skip(self, rx))]
    pub async fn listen_for_file_requests(&mut self, rx: Receiver<ProviderRequest>) {
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
            self.check_and_apply_changes().await;
            let result = match file_request {
                ProviderRequest::OpenFile(r) => self.open_file(r).await,
                ProviderRequest::ReleaseFile(r) => self.release_file(r).await,
                ProviderRequest::Metadata(r) => self.metadata(r).await,
                ProviderRequest::ReadContent(r) => self.read_content(r).await,
                ProviderRequest::WriteContent(r) => self.write_content(r).await,
                ProviderRequest::ReadDir(r) => self.read_dir(r).await,
                ProviderRequest::Rename(r) => self.rename(r).await,
                ProviderRequest::Lookup(r) => self.lookup(r).await,
                ProviderRequest::SetAttr(r) => self.set_attr(r).await,
                _ => {
                    error!(
                    "DriveFileProvider::listen_for_file_requests() received unknown request: {:?}",
                    file_request
                );
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

    async fn check_and_apply_changes(&mut self) {
        let changes = self.get_changes().await;
        if let Ok(changes) = changes {
            for change in changes {
                let change_applied_successful = self.process_change(change);
                if let Err(e) = change_applied_successful {
                    error!("got an error while applying change: {:?}", e);
                }
            }
        }
    }
    //endregion

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

        let result = self.find_first_child_by_name(&name, &parent_id);

        if let Some(result) = result {
            let result = Self::create_file_metadata_from_entry(result);
            let response = ProviderResponse::Lookup(Some(result));
            return send_response!(request, response);
        }

        debug!("could not find file: {} in {}", name, parent_id);
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
        // let entry = self.entries.get(file_id).context("could not get entry");
        // if let Err(e) = entry {
        //     return send_error_response!(request, e, libc::EIO);
        // }
        // let entry = entry.unwrap();
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
            OpenOptions::new()
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
    //region rename

    async fn rename(&mut self, request: ProviderRenameRequest) -> Result<()> {
        let original_parent = self.get_correct_id(request.original_parent.clone());
        let original_name = request.original_name.into_string();
        if let Err(e) = original_name {
            return send_error_response!(
                request,
                anyhow!("Could not convert original name into string: {:?}", e),
                libc::EIO
            );
        }
        let original_name = original_name.unwrap();
        let new_parent = self.get_correct_id(request.new_parent.clone());
        let new_name = request.new_name.into_string();
        if let Err(e) = new_name {
            return send_error_response!(
                request,
                anyhow!("Could not convert new name into string: {:?}", e),
                libc::EIO
            );
        }
        let new_name = new_name.unwrap();

        let rename_result = self
            .rename_inner(&original_parent, &original_name, &new_parent, &new_name)
            .await;
        if let Err((msg, code)) = rename_result {
            return send_error_response!(request, anyhow!("{}", msg), code);
        }

        send_response!(request, ProviderResponse::Rename)
    }

    async fn rename_inner(
        &mut self,
        original_parent: &DriveId,
        original_name: &String,
        new_parent: &DriveId,
        new_name: &String,
    ) -> StdResult<(), (String, c_int)> {
        let file_entry = self.find_first_child_by_name(&original_name, &original_parent);
        if file_entry.is_none() {
            return Err((format!("Could not find rename source"), libc::ENOENT));
        }
        let file_entry = file_entry.unwrap();

        let file_id = file_entry.get_id();
        if file_id.is_none() {
            return Err((format!("Could not get id from entry"), libc::EINVAL));
        }
        let file_id = file_id.unwrap();

        let wait_res = self
            .wait_for_running_drive_request_if_exists(&file_id)
            .await;
        if let Err(e) = wait_res {
            return Err((e.to_string(), libc::EIO));
        }

        if self.check_id_exists(&new_parent) {
            return Err((format!("Folder does not exist"), libc::ENOENT));
        }

        if self.does_target_name_exist_under_parent(&new_parent, &new_name) {
            return Err((format!("Target name is already used"), libc::EADDRINUSE));
        }

        let entry = self
            .entries
            .get_mut(&file_id)
            .expect("We checked shortly before if the entry exists");

        if original_name != new_name {
            //check if the filename has been changed and update it in the metadata and on google drive
            entry.changed_metadata.name = Some(new_name.clone());
        }
        let now = SystemTime::now();
        entry.attr.atime = now;
        entry.attr.mtime = now;
        //check if the path is changed (child-parent relationships) and modify them accordingly
        if original_parent != new_parent {
            entry.changed_metadata.parents = Some(vec![new_parent.to_string()]);
            self.remove_parent_child_relation(original_parent.clone(), file_id.clone());
            self.add_parent_child_relation(new_parent.clone(), file_id.clone());
        }

        let upload_result = self.update_remote_metadata(file_id).await;
        if let Err(e) = upload_result {
            return Err((
                format!("Error while uploading Metadata: {:?}", e),
                libc::EREMOTEIO,
            ));
        }

        Ok(())
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

    fn does_target_name_exist_under_parent(
        &self,
        new_parent: &&DriveId,
        new_name: &&String,
    ) -> bool {
        let new_file_entry = self.find_first_child_by_name(&new_name, &new_parent);
        return new_file_entry.is_some();
    }
    fn check_id_exists(&self, id: &DriveId) -> bool {
        self.entries.contains_key(id)
    }

    /// returns the first entry it finds with the specified name that is a child of the parent_id
    ///
    /// returns ```Option::None``` if none match/the parent does not have any children  
    fn find_first_child_by_name(&self, name: &String, parent_id: &DriveId) -> Option<&FileData> {
        let mut result = None;
        let children = self.children.get(&parent_id);
        for child in children.unwrap_or(&vec![]) {
            if let Some(child) = self.entries.get(child) {
                if child
                    .metadata
                    .name
                    .as_ref()
                    .unwrap_or(&"$'\\NO_NAME".to_string())
                    .eq_ignore_ascii_case(&name)
                {
                    result = Some(child);
                    break;
                }
            }
        }
        result
    }

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
            // } else {
            //     error!("File handle does not have a file");
            //     return Err(anyhow!("File handle does not have a file"));
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
        let file: &mut File = file;
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
            debug!(
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

    //region drive helpers
    #[instrument]
    async fn get_changes(&mut self) -> Result<Vec<Change>> {
        if self.last_checked_for_changes + self.allowed_cache_time > SystemTime::now() {
            debug!("not checking for changes since we already checked recently");
            return Ok(vec![]);
        }
        debug!("checking for changes...");
        let changes: Result<Vec<Change>> = self
            .drive
            .get_changes_since(&mut self.changes_start_token)
            .await?
            .into_iter()
            .map(Change::try_from)
            .collect();

        self.last_checked_for_changes = SystemTime::now();
        debug!(
            "checked for changes, found {} changes",
            changes.as_ref().unwrap_or(&Vec::<Change>::new()).len()
        );
        changes
    }

    async fn update_remote_metadata(&self, id: DriveId) -> Result<()> {
        let file_data = self.entries.get(&id);
        if file_data.is_none() {
            return Err(anyhow!("Could not get entry with id: {}", id));
        }
        let file_data = file_data.unwrap();
        let mut metadata = file_data.changed_metadata.clone();
        Self::prepare_changed_metadata_for_upload(&id, &mut metadata);
        self.drive
            .update_file_metadata_on_drive(metadata, &file_data.metadata);

        Ok(())
    }

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
        Self::prepare_changed_metadata_for_upload(&id, &mut metadata);
        metadata.mime_type = file_data.metadata.mime_type.clone();

        let target_path = self.construct_path(&id)?;
        debug!(
            "starting upload in the background for path: '{}' and metadata: {:?}",
            target_path.display(),
            metadata
        );
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

    fn prepare_changed_metadata_for_upload(id: &DriveId, mut metadata: &mut DriveFileMetadata) {
        metadata.id = Some(id.clone().into());
        remove_volatile_metadata(&mut metadata);
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
            self.add_drive_entry_to_entries(entry);
        }
        // for (i, (id, data)) in self.entries.iter().enumerate() {
        //     info!("entry {:3} id: {:>40} data: {:?}", i, id, data);
        // }
        Ok(())
    }

    fn add_drive_entry_to_entries(&mut self, entry: DriveFileMetadata) -> bool {
        let id = &entry.id;
        if let Some(id) = id {
            let id = DriveId::from(id);
            let attr = self.create_file_attr_from_metadata(&entry);
            if attr.is_err() {
                warn!(
                    "error while creating FileAttr from metadata: {:?} entry: {:?}",
                    attr, entry
                );
                return true;
            }
            let attr = attr.unwrap();
            self.add_child_parent_relations(&entry, &id);
            let entry_data = FileData {
                metadata: entry,
                changed_metadata: Default::default(),
                perma: false, //TODO: read the perma marker from somewhere (maybe only after all files have been checked?)
                attr,
                is_local: false,
            };
            self.entries.insert(id, entry_data);
        }
        false
    }

    fn add_child_parent_relations(&mut self, entry: &DriveFileMetadata, id: &DriveId) {
        if let Some(parents) = &entry.parents {
            for parent in parents {
                let parent_id = DriveId::from(parent);
                self.add_parent_child_relation(parent_id, id.clone());
            }
        } else {
            //file is at root level
            self.add_parent_child_relation(self.get_correct_id(DriveId::root()), id.clone());
        }
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

    /// changes alias ids like ```DriveId::root()``` into their actual IDs on the drive
    fn get_correct_id(&self, id: DriveId) -> DriveId {
        if id == DriveId::root() {
            trace!("aliasing DriveId::root() to actual root: {}", id);
            return self.alt_root_id.clone();
        }
        return id;
    }
    fn process_change(&mut self, change: Change) -> Result<()> {
        let id = change.id;
        let id = self.get_correct_id(id);

        let entry = self.entries.get_mut(&id);
        if let Some(entry) = entry {
            match change.kind {
                ChangeType::Drive(drive) => {
                    todo!("drive changes are not supported yet: {:?}", drive);
                }
                ChangeType::File(file_change) => {
                    //TODO: check if local has changes that conflict (content)
                    //TODO: check if the content was changed (checksum) and schedule
                    // a download if it is a local/perm file or mark it for download on next open
                    process_file_change(entry, file_change)?;
                }
                ChangeType::Removed => {
                    todo!("remove local file/dir since it was deleted on the remote");
                }
            }
            return Ok(());
        } else {
            todo!("there was a file/dir added on the remote since this ID is unknown")
        }
    }

    #[instrument(skip(self, file_change))]
    fn process_remote_file_moved(&mut self, id: &DriveId, file_change: &DriveFileMetadata) {
        if let Some(changed_parents) = &file_change.parents {
            trace!("parent was changed! {:?}", id);
            let entry = self.entries.get(&id);
            if let Some(entry) = entry {
                trace!(
                    "changed_parents: {:?} before change: {:?}",
                    changed_parents,
                    entry.metadata.parents
                );
                if Some(changed_parents) != entry.metadata.parents.as_ref() {
                    if let Some(existing_parents) = entry.metadata.parents.clone() {
                        for e in existing_parents {
                            let old_parent_id = self.get_correct_id(DriveId::from(&e));
                            trace!("(1) converted id from {} to {}", e, old_parent_id);
                            self.remove_parent_child_relation(old_parent_id, id.clone());
                        }
                    }
                    trace!("done removing old parents");
                    for new_parent in changed_parents {
                        let new_parent_id = self.get_correct_id(DriveId::from(new_parent));
                        trace!("(2) converted id from {} to {} ", new_parent, new_parent_id);
                        self.add_parent_child_relation(new_parent_id, id.clone());
                    }
                    trace!("done adding new parents");
                    let entry_m = self.entries.get_mut(id);
                    if let Some(entry_m) = entry_m {
                        entry_m.metadata.parents = file_change.parents.clone();
                    }
                    trace!("done modifying metadata");
                } else {
                    trace!(
                        "before and after are equal: {:?} == {:?}",
                        Some(changed_parents),
                        entry.metadata.parents
                    );
                }
            } else {
                warn!(
                    "A remote file was moved but is unknown locally. is this right?: {:?}",
                    id
                );
            }
        }
    }
}
#[instrument]
fn process_file_change(entry: &mut FileData, change: DriveFileMetadata) -> Result<()> {
    if let Some(size) = change.size {
        entry.metadata.size = Some(size);
        entry.attr.size = size as u64;
        //TODO1: set the size of the cached file if necessary
    }
    if let Some(name) = change.name {
        entry.metadata.name = Some(name);
    }
    if let Some(parents) = change.parents {
        if Some(&parents) != entry.metadata.parents.as_ref() {
            //TODO1: change the parent child relations
            warn!(
                "parents changed from {:?}: {:?}",
                entry.metadata.parents,
                Some(parents)
            )
        }
    }
    if let Some(description) = change.description {
        entry.metadata.description = Some(description);
    }
    if let Some(thumbnail_link) = change.thumbnail_link {
        entry.metadata.thumbnail_link = Some(thumbnail_link);
    }
    warn!("not all changes have been implemented");
    //TODO2: implement all other needed changes!
    // if let Some() = change.{
    //      entry.metadata. = ;
    // }
    Ok(())
}

fn remove_volatile_metadata(metadata: &mut DriveFileMetadata) {
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
    // metadata.parents = None;
    // parents have to be set differently: "The parents field is not directly writable in update requests. Use the addParents and removeParents parameters instead."
    metadata.kind = None;
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
