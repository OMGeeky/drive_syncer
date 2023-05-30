use crate::google_drive::DriveId;
use crate::prelude::*;
use fuser::FileAttr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DriveEntry {
    pub id: DriveId,

    pub name: String,
    pub local_path: Option<PathBuf>,
    pub attr: FileAttr,
    pub drive_metadata: Option<DriveFileMetadata>,
    pub has_upstream_content_changes: bool,
    pub md5_checksum: Option<String>,
    pub local_md5_checksum: Option<String>,
}
