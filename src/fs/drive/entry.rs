use std::ffi::OsString;
use std::path::PathBuf;

use fuser::FileAttr;
use tracing::instrument;

use crate::common::LocalPath;
use crate::fs::Inode;
use crate::google_drive::DriveId;

#[derive(Debug, Clone)]
pub struct DriveEntry {
    pub ino: Inode,
    pub drive_id: DriveId,

    pub name: OsString,
    // pub drive_path: OsString,
    pub local_path: Option<LocalPath>,
    pub attr: FileAttr,
    pub drive_metadata: Option<google_drive3::api::File>,
    pub has_upstream_content_changes: bool,
    pub md5_checksum: Option<String>,
    pub local_md5_checksum: Option<String>,
}

impl DriveEntry {
    #[instrument]
    pub(crate) fn set_md5_checksum(&mut self, md5_checksum: Option<String>) {
        self.md5_checksum = md5_checksum.clone();
        self.local_md5_checksum = md5_checksum;
    }
}

impl DriveEntry {
    pub fn new(
        ino: impl Into<Inode>,
        name: impl Into<OsString>,
        drive_id: impl Into<DriveId>,

        // local_path: impl Into<LocalPath>,
        attr: FileAttr,
        drive_metadata: Option<google_drive3::api::File>,
    ) -> Self {
        let name = name.into();
        // let path = local_path.into();
        Self {
            ino: ino.into(),
            drive_id: drive_id.into(),
            name,
            // drive_path: path.clone().into(),
            local_path: None,
            attr,
            drive_metadata,
            has_upstream_content_changes: true,
            md5_checksum: None,
            local_md5_checksum: None,
        }
    }
    pub fn build_local_path(&mut self, parent: Option<LocalPath>) {
        if let Some(parent_path) = parent {
            let path = parent_path.join(&self.name);
            self.local_path = Some(LocalPath::from(path));
        } else {
            self.local_path = Some(LocalPath::from(PathBuf::from("")));
        }
    }
}
// impl CommonEntry for DriveEntry {
//     fn get_ino(&self) -> Inode {
//         self.ino
//     }
//
//     fn get_name(&self) -> &OsStr {
//         &self.name
//     }
//
//     fn get_local_path(&self) -> &LocalPath {
//         &self.local_path
//     }
//
//     fn get_attr(&self) -> &FileAttr {
//         &self.attr
//     }
// }
