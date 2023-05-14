use crate::common::LocalPath;
use crate::fs::{CommonEntry, Inode};
use crate::google_drive::DriveId;
use fuser::FileAttr;
use std::ffi::{OsStr, OsString};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveEntry {
    pub ino: Inode,
    pub drive_id: DriveId,

    pub name: OsString,
    // pub drive_path: OsString,
    pub local_path: LocalPath,
    pub attr: FileAttr,
}
impl DriveEntry {
    pub fn new(
        ino: impl Into<Inode>,
        name: impl Into<OsString>,
        drive_id: impl Into<DriveId>,

        local_path: impl Into<LocalPath>,
        attr: FileAttr,
    ) -> Self {
        let name = name.into();
        let path = local_path.into();
        Self {
            ino: ino.into(),
            drive_id: drive_id.into(),
            name,
            // drive_path: path.clone().into(),
            local_path: path,
            attr,
        }
    }
}
impl CommonEntry for DriveEntry {
    fn get_ino(&self) -> Inode {
        self.ino
    }

    fn get_name(&self) -> &OsStr {
        &self.name
    }

    fn get_local_path(&self) -> &LocalPath {
        &self.local_path
    }

    fn get_attr(&self) -> &FileAttr {
        &self.attr
    }
}
