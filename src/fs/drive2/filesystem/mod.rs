use std::{
    collections::HashMap,
    ffi::OsStr,
    fmt::{Display, Formatter},
    sync::mpsc::{channel, Receiver, Sender},
    time::{Duration, SystemTime},
};

use anyhow::{anyhow, Context};
use bimap::BiMap;
use fuser::{
    FileAttr, Filesystem, KernelConfig, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow,
};
use libc::c_int;
use tokio::fs::File;
use tracing::{debug, error, field::debug, instrument, trace};

pub use handle_flags::HandleFlags;

use crate::fs::drive_file_provider::{
    ProviderLookupRequest, ProviderMetadataRequest, ProviderOpenFileRequest,
    ProviderReadContentRequest, ProviderReadDirRequest, ProviderReleaseFileRequest,
    ProviderRenameRequest, ProviderRequest, ProviderResponse, ProviderSetAttrRequest,
    ProviderWriteContentRequest,
};
use crate::google_drive::DriveId;
use crate::{
    match_provider_response, prelude::*, receive_response, reply_error_e, reply_error_e_consuming,
    reply_error_o, send_request,
};

//TODO2: decide if 1 second is a good TTL for all cases
const TTL: Duration = Duration::from_secs(2);

mod handle_flags;

#[derive(Debug)]
struct FileHandleData {
    flags: HandleFlags,
}

#[derive(Debug)]
struct Entry {
    attr: FileAttr,
}

#[derive(Debug)]
pub struct DriveFilesystem {
    file_provider_sender: tokio::sync::mpsc::Sender<ProviderRequest>,

    entry_ids: BiMap<u64, DriveId>,
    ino_to_file_handles: HashMap<u64, Vec<u64>>,
    next_ino: u64,
}
//region DriveFilesystem ino_to_file_handle
impl DriveFilesystem {
    fn get_fh_from_ino(&self, ino: u64) -> Option<&Vec<u64>> {
        self.ino_to_file_handles.get(&ino)
    }
    fn get_ino_from_fh(&self, fh: u64) -> Option<u64> {
        for (ino, fhs) in self.ino_to_file_handles.iter() {
            if fhs.contains(&fh) {
                return Some(*ino);
            }
        }
        None
    }
    fn remove_fh(&mut self, fh: u64) -> Result<()> {
        let ino = self
            .get_ino_from_fh(fh)
            .context("could not find ino for fh")?;

        let x = self
            .ino_to_file_handles
            .get_mut(&ino)
            .context("could not find fh for ino")?;
        x.retain(|&x| x != fh);
        // let data = self
        //     .file_handles
        //     .remove(&fh)
        //     .context("could not find handle data for fh")?;
        // Ok(data)
        Ok(())
    }
    fn add_fh(&mut self, ino: u64, fh: u64, handle: FileHandleData) -> Result<()> {
        let fhs = self.ino_to_file_handles.get_mut(&ino); //.or_insert_with(||vec![fh]);
        if let Some(fhs) = fhs {
            if !fhs.contains(&fh) {
                fhs.push(fh);
            } else {
                error!("fh {} already exists for ino {}", fh, ino);
                return Err(anyhow!("fh {} already exists for ino {}", fh, ino));
            }
        } else {
            self.ino_to_file_handles.insert(ino, vec![fh]);
        }
        debug!("added fh {} to ino {}", fh, ino);
        Ok(())
    }
}
//endregion
//region DriveFilesystem ino_to_id
impl DriveFilesystem {
    fn get_id_from_ino(&self, ino: u64) -> Option<&DriveId> {
        self.entry_ids.get_by_left(&ino)
    }
    fn get_ino_from_id(&mut self, id: DriveId) -> u64 {
        let x = self.entry_ids.get_by_right(&id);
        if let Some(ino) = x {
            return *ino;
        }
        self.add_id(id)
    }
    fn remove_id(&mut self, id: DriveId) -> Result<u64> {
        if let Some((ino, _)) = self.entry_ids.remove_by_right(&id) {
            Ok(ino)
        } else {
            Err(anyhow!("could not find id {}", id))
        }
    }
    fn add_id(&mut self, id: DriveId) -> u64 {
        let ino = self.generate_ino();
        trace!("adding new ino for drive id: {} => {}", id, ino);
        self.entry_ids.insert(ino, id);
        ino
    }
}
//endregion
impl Display for DriveFilesystem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DriveFilesystem(entry ids: {})", self.entry_ids.len())
    }
}

impl DriveFilesystem {
    pub fn new(file_provider_sender: tokio::sync::mpsc::Sender<ProviderRequest>) -> Self {
        Self {
            file_provider_sender,
            entry_ids: BiMap::new(),
            ino_to_file_handles: HashMap::new(),
            next_ino: 222,
        }
    }
    fn generate_ino(&mut self) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        ino
    }
}

impl Filesystem for DriveFilesystem {
    //region init
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut KernelConfig,
    ) -> std::result::Result<(), c_int> {
        self.entry_ids.insert(1, DriveId::from("root"));
        Ok(())
    }
    //endregion
    //region lookup
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);

        let parent_id = self.entry_ids.get_by_left(&parent);
        reply_error_o!(
            parent_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for parent ino: {}",
            parent
        );

        let v = ProviderRequest::Lookup(ProviderLookupRequest::new(
            parent_id,
            name.to_os_string(),
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);

        receive_response!(provider_rx, response, reply);
        match_provider_response!(response, reply, ProviderResponse::Lookup(metadata), {
            if let Some(metadata) = metadata {
                let mut attr = metadata.attr;
                attr.ino = self.get_ino_from_id(metadata.id);
                reply.entry(&TTL, &attr, 0); //TODO3: generation
            } else {
                reply.error(libc::ENOENT);
            }
        });
        debug!("done with lookup!");
    }
    //endregion
    //region getattr
    #[instrument(skip(_req), fields(% self))]
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let drive_id = self.entry_ids.get_by_left(&ino);
        reply_error_o!(
            drive_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            ino
        );
        debug!("getting attributes");

        let v = ProviderRequest::Metadata(ProviderMetadataRequest::new(drive_id, provider_res_tx));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);
        match_provider_response!(response, reply, ProviderResponse::Metadata(metadata), {
            trace!("Received ProviderResponse::Metadata({:?})", metadata);
            let mut attr = metadata.attr;
            attr.ino = ino;
            trace!("responding with attr: {:?}", attr);
            reply.attr(&TTL, &attr);
        });
    }
    //endregion
    //region setattr
    #[instrument(skip(_req), fields(% self))]
    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let drive_id = self.entry_ids.get_by_left(&ino);
        reply_error_o!(
            drive_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            ino
        );
        debug!("getting attributes");
        let v = ProviderRequest::SetAttr(ProviderSetAttrRequest::new(
            drive_id,
            mode,
            uid,
            gid,
            size,
            flags,
            fh,
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);
        match_provider_response!(response, reply, ProviderResponse::SetAttr(metadata), {
            trace!("Received ProviderResponse::SetAttr({:?})", metadata);
            let mut attr = metadata.attr;
            attr.ino = ino;
            trace!("responding with attr: {:?}", attr);
            reply.attr(&TTL, &attr);
        });
    }
    //endregion
    //region open
    #[instrument(skip(_req), fields(%self))]
    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        // let fh_id = self.generate_fh();
        // // let flags = HandleFlags::from(flags);
        // let handle_data = FileHandleData { flags };
        // self.add_fh(ino, fh_id, handle_data);

        let drive_id = self.entry_ids.get_by_left(&ino);
        reply_error_o!(
            drive_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            ino
        );
        let v = ProviderRequest::OpenFile(ProviderOpenFileRequest::new(
            drive_id,
            flags,
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);
        match_provider_response!(response, reply, ProviderResponse::OpenFile(fh, flags), {
            trace!("got OpenFile result: fh: {}, flags: {:?}", fh, flags);
            let x = self.ino_to_file_handles.get_mut(&ino);
            if let Some(x) = x {
                x.push(fh);
            } else {
                self.ino_to_file_handles.insert(ino, vec![fh]);
            }
            reply.opened(fh, flags.into());
        });
    }
    //endregion
    //region read
    #[instrument(skip(_req), fields(% self))]
    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let drive_id = self.entry_ids.get_by_left(&ino);
        reply_error_o!(
            drive_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            ino
        );

        let v = ProviderRequest::ReadContent(ProviderReadContentRequest::new(
            drive_id,
            offset as u64,
            size as usize,
            fh,
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);
        match_provider_response!(response, reply, ProviderResponse::ReadContent(content), {
            reply.data(content.as_slice());
            trace!("Received ProviderResponse::Ok");
        });
    }
    //endregion
    //region write
    #[instrument(skip(_req), fields(% self, data = data.len()))]
    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        write_flags: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let drive_id = self.entry_ids.get_by_left(&ino);
        reply_error_o!(
            drive_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            ino
        );
        let v = ProviderRequest::WriteContent(ProviderWriteContentRequest::new(
            drive_id,
            offset as u64,
            fh,
            data.to_vec(),
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);
        match_provider_response!(response, reply, ProviderResponse::WriteSize(content), {
            reply.written(content);
            trace!("Received ProviderResponse::WriteSize({})", content);
        });
    }
    //endregion
    //region release
    #[instrument(skip(_req), fields(%self))]
    fn release(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let drive_id = self.get_id_from_ino(ino);
        reply_error_o!(
            drive_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            ino
        );

        let v = ProviderRequest::ReleaseFile(ProviderReleaseFileRequest::new(
            drive_id.clone(),
            fh,
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);
        match_provider_response!(response, reply, ProviderResponse::ReleaseFile, {
            let handle_data = self.remove_fh(fh);
            reply_error_e_consuming!(
                handle_data,
                reply,
                libc::ENOENT,
                "Failed to find file_handle for fh: {}",
                fh
            );
            reply.ok();
            debug!("Released file_handle for fh: {}", fh);
        });
    }
    //endregion
    //region readdir
    #[instrument(skip(_req, reply), fields(% self))]
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let drive_id = self.entry_ids.get_by_left(&ino);
        reply_error_o!(
            drive_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            ino
        );

        let v = ProviderRequest::ReadDir(ProviderReadDirRequest::new(
            drive_id,
            offset as u64,
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);

        match_provider_response!(response, reply, ProviderResponse::ReadDir(response), {
            let mut counter = 0;
            debug!(
                "received ProviderReadDirResponse with {} entries",
                response.entries.len()
            );
            for entry in response.entries {
                let entry_ino = self.get_ino_from_id(entry.id.clone());
                counter += 1;
                debug!(
                    "adding entry to output: ino:{}, counter:{}, entry: {:?}",
                    entry_ino, counter, entry
                );
                let buffer_full = reply.add(entry_ino, counter, entry.attr.kind, &entry.name);
                if buffer_full {
                    debug!("buffer full after {}", counter);
                    break;
                }
            }
            debug!("sending ok");
            reply.ok();
            return;
        });
    }

    //endregion
    //region rename
    #[instrument(skip(_req, reply, _flags), fields(% self))]
    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let (provider_res_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let parent_id = self.get_id_from_ino(parent);
        reply_error_o!(
            parent_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            parent
        );
        let new_parent_id = self.get_id_from_ino(new_parent);
        reply_error_o!(
            new_parent_id,
            reply,
            libc::ENOENT,
            "Failed to find drive_id for ino: {}",
            new_parent
        );

        let v = ProviderRequest::Rename(ProviderRenameRequest::new(
            name.to_os_string(),
            parent_id.clone(),
            new_name.to_os_string(),
            new_parent_id.clone(),
            provider_res_tx,
        ));
        send_request!(self.file_provider_sender, v, reply);
        receive_response!(provider_rx, response, reply);

        match_provider_response!(response, reply, ProviderResponse::Rename, {
            //
            debug!("Sending Ok.")
            reply.ok();
        });
    }
    //endregion
}
