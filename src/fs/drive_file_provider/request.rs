use std::ffi::OsString;
use std::fmt::Debug;
use std::path::PathBuf;

use anyhow::Error;
use fuser::{FileAttr, Filesystem};
use libc::c_int;
use tokio::sync::mpsc::Sender;

use crate::fs::drive2::HandleFlags;
use crate::fs::drive_file_provider::FileHandleData;
use crate::google_drive::DriveId;
use crate::prelude::*;

#[derive(Debug)]
pub enum ProviderResponse {
    OpenFile(u64, HandleFlags),
    ReleaseFile,
    SetAttr(FileMetadata),
    Metadata(FileMetadata),
    Lookup(Option<FileMetadata>),
    ReadContent(Vec<u8>),
    ReadDir(ProviderReadDirResponse),
    WriteSize(u32),
    // Ok,
    Error(Error, c_int),
    Unknown,
}

#[derive(Debug)]
pub enum ProviderRequest {
    OpenFile(ProviderOpenFileRequest),
    Lookup(ProviderLookupRequest),
    ReleaseFile(ProviderReleaseFileRequest),
    Metadata(ProviderMetadataRequest),
    SetAttr(ProviderSetAttrRequest),
    ReadContent(ProviderReadContentRequest),
    ReadDir(ProviderReadDirRequest),
    WriteContent(ProviderWriteContentRequest),
    Unknown,
}
pub trait ProviderRequestStruct {
    fn get_file_id(&self) -> &DriveId;
    fn get_response_sender(&self) -> &Sender<ProviderResponse>;
}
//region ProviderRequest structs
#[derive(Debug)]
pub struct ProviderMetadataRequest {
    pub file_id: DriveId,
    pub response_sender: Sender<ProviderResponse>,
}

impl ProviderMetadataRequest {
    pub(crate) fn new(id: impl Into<DriveId>, response_sender: Sender<ProviderResponse>) -> Self {
        Self {
            file_id: id.into(),
            response_sender,
        }
    }
}

#[derive(Debug)]
pub struct ProviderSetAttrRequest {
    pub file_id: DriveId,

    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,

    pub flags: Option<u32>,
    pub fh: Option<u64>,
    pub response_sender: Sender<ProviderResponse>,
}

impl ProviderSetAttrRequest {
    pub(crate) fn new(
        id: impl Into<DriveId>,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        flags: Option<u32>,
        fh: Option<u64>,
        response_sender: Sender<ProviderResponse>,
    ) -> Self {
        Self {
            file_id: id.into(),
            mode,
            uid,
            gid,
            size,
            flags,
            fh,
            response_sender,
        }
    }
}

impl ProviderRequestStruct for ProviderMetadataRequest {
    fn get_file_id(&self) -> &DriveId {
        &self.file_id
    }

    fn get_response_sender(&self) -> &Sender<ProviderResponse> {
        &self.response_sender
    }
}

#[derive(Debug)]
pub struct ProviderOpenFileRequest {
    pub file_id: DriveId,
    pub flags: i32,
    pub response_sender: Sender<ProviderResponse>,
}

#[derive(Debug)]
pub struct ProviderLookupRequest {
    pub name: OsString,
    pub parent: DriveId,
    pub response_sender: Sender<ProviderResponse>,
}

impl ProviderOpenFileRequest {
    pub(crate) fn new(
        id: impl Into<DriveId>,
        flags: i32,
        response_sender: Sender<ProviderResponse>,
    ) -> Self {
        Self {
            file_id: id.into(),
            flags,
            response_sender,
        }
    }
}
impl ProviderLookupRequest {
    pub(crate) fn new(
        parent_id: impl Into<DriveId>,
        name: OsString,
        response_sender: Sender<ProviderResponse>,
    ) -> Self {
        Self {
            parent: parent_id.into(),
            name,
            response_sender,
        }
    }
}
//
// impl ProviderRequestStruct for ProviderOpenFileRequest {
//     fn get_file_id(&self) -> &DriveId {
//         &self.file_id
//     }
//
//     fn get_response_sender(&self) -> &tokio::sync::mpsc::Sender<ProviderResponse> {
//         &self.response_sender
//     }
// }
#[derive(Debug)]
pub struct ProviderReleaseFileRequest {
    pub file_id: DriveId,
    pub fh: u64,
    // pub flags: u32,
    // pub lock_owner: u64,
    // pub flush: bool,
    pub response_sender: Sender<ProviderResponse>,
}

impl ProviderReleaseFileRequest {
    pub fn new(id: DriveId, fh: u64, response_sender: Sender<ProviderResponse>) -> Self {
        Self {
            file_id: id,
            fh,
            response_sender,
        }
    }
}

#[derive(Debug)]
pub struct ProviderReadContentRequest {
    pub file_id: DriveId,
    pub offset: u64,
    pub size: usize,
    pub fh: u64,
    pub response_sender: Sender<ProviderResponse>,
}
impl ProviderRequestStruct for ProviderReadContentRequest {
    fn get_file_id(&self) -> &DriveId {
        &self.file_id
    }

    fn get_response_sender(&self) -> &Sender<ProviderResponse> {
        &self.response_sender
    }
}

impl ProviderReadContentRequest {
    pub(crate) fn new(
        id: impl Into<DriveId>,
        offset: u64,
        size: usize,
        fh: u64,
        response_sender: Sender<ProviderResponse>,
    ) -> Self {
        Self {
            file_id: id.into(),
            offset,
            size,
            fh,
            response_sender,
        }
    }
}

#[derive(Debug)]
pub struct ProviderReadDirRequest {
    pub file_id: DriveId,
    pub offset: u64,
    pub response_sender: Sender<ProviderResponse>,
}

impl ProviderRequestStruct for ProviderReadDirRequest {
    fn get_file_id(&self) -> &DriveId {
        &self.file_id
    }

    fn get_response_sender(&self) -> &Sender<ProviderResponse> {
        &self.response_sender
    }
}
impl ProviderReadDirRequest {
    pub(crate) fn new(
        id: impl Into<DriveId>,
        offset: u64,
        response_sender: Sender<ProviderResponse>,
    ) -> Self {
        Self {
            file_id: id.into(),
            offset,
            response_sender,
        }
    }
}

#[derive(Debug)]
pub struct ProviderWriteContentRequest {
    pub file_id: DriveId,
    pub offset: u64,
    pub fh: u64,
    pub data: Vec<u8>,
    pub response_sender: Sender<ProviderResponse>,
}

impl ProviderRequestStruct for ProviderWriteContentRequest {
    fn get_file_id(&self) -> &DriveId {
        &self.file_id
    }

    fn get_response_sender(&self) -> &Sender<ProviderResponse> {
        &self.response_sender
    }
}
impl ProviderWriteContentRequest {
    pub(crate) fn new(
        id: impl Into<DriveId>,
        offset: u64,
        fh: u64,
        data: Vec<u8>,
        response_sender: Sender<ProviderResponse>,
    ) -> Self {
        Self {
            file_id: id.into(),
            offset,
            fh,
            data,
            response_sender,
        }
    }
}

// endregion
//region ProviderResponse structs

pub struct ProviderReadDirResponse {
    pub entries: Vec<FileMetadata>,
}
impl Debug for ProviderReadDirResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderReadDirResponse").finish()
    }
}
//endregion
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub id: DriveId,
    pub name: String,
    // pub local_path: Option<PathBuf>,
    pub attr: FileAttr,
    // md5_checksum: Option<String>,
}
