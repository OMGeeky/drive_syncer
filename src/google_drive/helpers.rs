use crate::fs::drive::DriveFilesystem;
use crate::fs::{CommonFilesystem, Inode};
use crate::google_drive::{DriveId, GoogleDrive};
use crate::prelude::*;
use anyhow::anyhow;
use drive3::api::File;
use log::debug;
use mime::Mime;
use std::path::{Path, PathBuf};
use std::str::FromStr;
pub fn get_mime_from_file_metadata(file: &File) -> Result<Mime> {
    Ok(Mime::from_str(
        &file.mime_type.as_ref().unwrap_or(&"*/*".to_string()),
    )?)
}
pub fn get_drive_id_from_local_path(drive: &DriveFilesystem, path: &Path) -> Result<DriveId> {
    let drive_mount_point: &PathBuf = &drive.get_root_path().into();
    debug!("get_drive_id_from_path(): (0) path: '{}'", path.display());
    let path = match path.strip_prefix(drive_mount_point) {
        Err(e) => {
            return Err(anyhow!(
                "Path {:?} is not a prefix of {:?}",
                drive_mount_point,
                path
            ))?
        }
        Ok(path) => path,
    };
    debug!("get_drive_id_from_path(): (1) path: '{}'", path.display());
    if path == Path::new("/") || path == Path::new("") {
        debug!(
            "get_drive_id_from_path(): (1) path is root: '{}'",
            path.display()
        );
        return Ok("root".into());
    }

    let mut parent_ino: Inode = 5u32.into();
    // let mut parent_ino : Inode =Inode::from(5u32);//.into();
    for part in path.iter() {
        debug!("get_drive_id_from_path(): (2..) path: '{:?}'", part);

        let children = drive.get_children().get(&parent_ino);
        debug!("get_drive_id_from_path(): (2..) children: '{:?}'", children);
    }
    todo!("get_drive_id_from_path()")
}
mod test {
    use super::*;
    #[tokio::test]
    async fn test_get_drive_id_from_local_path() {
        crate::init_logger();
        let path = Path::new("/drive1");
        let drive = DriveFilesystem::new(path).await;
        let drive_mount_point = Path::new("/drive1");

        let drive_id = get_drive_id_from_local_path(&drive, path).unwrap();
        assert_eq!(drive_id, "root".into());

        let path = Path::new("/drive1/");
        let drive_id = get_drive_id_from_local_path(&drive, path).unwrap();
        assert_eq!(drive_id, "root".into());

        let path = Path::new("/drive1/dir1/dir2/file1.txt");
        let drive_id = get_drive_id_from_local_path(&drive, path).unwrap();
        todo!("create assert for this test");
        // assert_eq!(drive_id, "TODO".into());
    }
}
