#![allow(dead_code, unused)]

extern crate google_drive3 as drive3;

use std::error::Error;
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, SystemTime};
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use drive3::{DriveHub, hyper, hyper_rustls, oauth2};
use drive3::api::Channel;
use fuser::{FileAttr, Filesystem, FileType, FUSE_ROOT_ID, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, ReplyXattr, Request, Session, SessionUnmounter, TimeOrNow};
use google_drive3::oauth2::read_application_secret;
// use nix;
use notify::{INotifyWatcher, recommended_watcher, RecommendedWatcher};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, stdin};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{channel, Sender};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, trace, warn};

use prelude::*;

use crate::config::common_file_filter::CommonFileFilter;
use crate::fs::drive::{DriveFilesystem, DriveFileUploader, FileUploaderCommand, SyncSettings};
use crate::fs::sample::SampleFilesystem;
use crate::google_drive::GoogleDrive;

pub mod async_helper;
pub mod common;
pub mod fs;
pub mod google_drive;
pub mod prelude;
pub mod config;

#[cfg(test)]
mod tests {
    use super::*;

    fn init_logger() {
        todo!("init logger (tracing)")
    }

    #[tokio::test]
    async fn does_it_work() {
        init_logger();
        list_files().await;
    }
}

pub async fn sample() -> Result<()> {
    //Test file id: "1IotISYu3cF7JrOdfFPKNOkgYg1-ii5Qs"
    list_files().await
}

async fn list_files() -> Result<()> {
    debug!("Hello, world!");
    let secret: oauth2::ApplicationSecret = read_application_secret("auth/client_secret.json")
        .await
        .expect("failed to read client secret file");
    let auth = oauth2::InstalledFlowAuthenticator::builder(
        secret,
        oauth2::InstalledFlowReturnMethod::HTTPRedirect,
    )
        .persist_tokens_to_disk("auth/token_store.json")
        .build()
        .await?;

    let hub = DriveHub::new(
        hyper::Client::builder().build(
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .build(),
        ),
        auth,
    );

    let result = hub
        .files()
        .get("1IotISYu3cF7JrOdfFPKNOkgYg1-ii5Qs")
        .doit()
        .await?;
    // debug!("Result: {:?}", result);
    let (body, file) = result;

    debug!("Body: {:?}", body);
    debug!("File: {:?}", file);

    // let result = hub.files().list().corpus("user").doit().await;

    // debug!("Result: {:?}", result);
    info!("Filename: {:?}", file.name.unwrap_or("NO NAME".to_string()));
    info!(
        "Description: {:?}",
        file.description.unwrap_or("NO DESCRIPTION".to_string())
    );
    Ok(())
}

#[derive(Default)]
struct MyFS {
    /// how long the responses can/should be cached
    time_to_live: Duration,

    main_ino: u64,
    main_size: u64,
    main_blksize: u64,
    main_uid: u32,
    main_gid: u32,
    main_flags: u32,
    main_content: Vec<u8>,
    main_file_type: Option<FileType>,
    main_name: String,
}

struct DirEntry {
    ino: u64,
    name: String,
    file_type: FileType,
}

impl MyFS {
    fn get_attr(&self, ino: u64) -> Option<FileAttr> {
        // Get the file attributes based on the inode number
        if ino == FUSE_ROOT_ID {
            Some(FileAttr {
                ino: FUSE_ROOT_ID,
                size: 0,
                blocks: 0,
                atime: UNIX_EPOCH,
                mtime: UNIX_EPOCH,
                ctime: UNIX_EPOCH,
                crtime: UNIX_EPOCH,
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 0,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: 0,
                flags: 0,
            })
        } else if ino == self.main_ino {
            Some(FileAttr {
                ino: FUSE_ROOT_ID,
                size: self.main_size,
                blocks: 0,
                atime: UNIX_EPOCH,
                mtime: UNIX_EPOCH,
                ctime: UNIX_EPOCH,
                crtime: UNIX_EPOCH,
                kind: self.main_file_type.unwrap_or(FileType::RegularFile),
                perm: 0o755,
                nlink: 0,
                uid: self.main_uid,
                gid: self.main_gid,
                rdev: 0,
                blksize: self.main_blksize as u32,
                flags: self.main_flags,
            })
        } else {
            None
        }
    }
    fn set_attr(
        &mut self,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        flags: Option<u32>,
    ) -> Option<FileAttr> {
        debug!(
            "set_attr=> ino: {}; mode: {:?}; uid: {:?}; gid: {:?}; size: {:?}; flags: {:?}",
            ino, mode, uid, gid, size, flags
        );
        // Get the file attributes based on the inode number
        if ino == self.main_ino {
            self.main_size = size.unwrap_or(self.main_size);
            self.main_flags = flags.unwrap_or(self.main_flags);
            self.main_uid = uid.unwrap_or(self.main_uid);
            self.main_gid = gid.unwrap_or(self.main_gid);
            return self.get_attr(ino);
        } else {
            None
        }
    }
    fn write_file(
        &mut self,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        flags: i32,
    ) -> Option<usize> {
        // Write the file and reply with the number of bytes written
        debug!(
            "write_file=> ino: {}; fh: {}; offset: {}; data: {:?}; flags: {}",
            ino, fh, offset, data, flags
        );
        if ino == self.main_ino {
            self.main_content = data.to_vec();
            // todo!("write the file and reply with the number of bytes written");
            return Some(data.len());
        } else {
            None
        }
    }
    fn read_file(&self, ino: u64, fh: u64, offset: i64, size: u32) -> Option<Vec<u8>> {
        debug!(
            "read_file=> ino: {}; fh: {}; offset: {}; size: {}",
            ino, fh, offset, size
        );
        if ino == self.main_ino {
            // Read the file and reply with the data
            let data = &self.main_content.clone(); //b"Hello World!";
            let offset_usize = offset as usize;
            let size_usize = size as usize;
            if data.len() <= offset_usize {
                let result = vec![libc::EOF as u8];
                debug!("read_file=> (0) result: {:?}", result);
                return Some(result);
            }
            if offset_usize + size_usize > data.len() {
                //return the rest of the data + EOF
                let mut result = data[1..].to_vec();
                result.push(libc::EOF as u8);
                debug!("read_file=> (1) result: {:?}", result);
                return Some(result);
                // todo!("output the rest of the data + EOF, not just EOF");
                return None;
            }

            let result = data[offset_usize..offset_usize + size_usize].to_vec();
            debug!("read_file=> (2) result: {:?}", result);
            return Some(result);
        } else {
            None
        }
    }
    fn read_dir(&self, ino: u64) -> Option<Vec<DirEntry>> {
        if ino == FUSE_ROOT_ID {
            let mut entries = Vec::new();

            let dir_entry = DirEntry {
                ino: self.main_ino,
                name: self.main_name.clone(),
                file_type: self.main_file_type.unwrap_or(FileType::RegularFile),
            };

            entries.push(dir_entry);
            Some(entries)
        } else {
            None
        }
    }
}

#[async_trait]
impl Filesystem for MyFS {
    fn open(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: ReplyOpen) {
        if _ino == self.main_ino {
            reply.opened(0, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }
    fn access(&mut self, _req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        if ino == self.main_ino {
            reply.ok()
        } else {
            reply.error(libc::ENOENT)
        }
    }
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if let Some(attr) = self.get_attr(ino) {
            reply.attr(&self.time_to_live, &attr);
        } else {
            reply.error(libc::ENOENT);
        }
    }
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
        if let Some(size) = self.write_file(ino, fh, offset, data, flags) {
            reply.written(size as u32);
        } else {
            reply.error(libc::ENOENT);
        }
    }
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
        if let Some(attr) = self.set_attr(ino, mode, uid, gid, size, flags) {
            reply.attr(&self.time_to_live, &attr);
        } else {
            reply.error(libc::ENOENT);
        }
    }
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
        if let Some(data) = self.read_file(ino, fh, offset, size) {
            let data = data.as_slice();
            reply.data(data);
        } else {
            reply.error(libc::ENOENT);
        }
    }
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir=> ino: {}; fh: {}; offset: {}", ino, fh, offset);
        if let Some(entries) = self.read_dir(ino) {
            for (i, entry) in entries.iter().enumerate().skip(offset as usize) {
                if reply.add(entry.ino, (i + 1) as i64, entry.file_type, &entry.name) {
                    break;
                }
            }
            reply.ok();
        } else {
            reply.error(libc::ENOENT);
        }
    }
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let main_path = OsStr::new(&self.main_name);
        debug!(
            "lookup=> parent: {}; name: {:?}; main_path: {:?}",
            parent, name, main_path
        );
        if name.eq_ignore_ascii_case(main_path) {
            let attr = self.get_attr(self.main_ino).unwrap();
            reply.entry(&self.time_to_live, &attr, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }
}

pub async fn watch_file_reading() -> Result<()> {
    let mountpoint = "/tmp/fuse/1";
    let options = vec![
        MountOption::RW,
        // MountOption::FSName("myfs".to_string()),
        // MountOption::AllowOther,
        // MountOption::AutoUnmount,
    ];
    debug!("Mounting fuse filesystem at {}", mountpoint);
    fuser::mount2(
        MyFS {
            time_to_live: Duration::from_secs(5),
            main_ino: 2,
            main_name: "1.txt".to_string(),
            main_file_type: Some(FileType::RegularFile),
            main_content: b"Hello World!".to_vec(),
            ..Default::default()
        },
        mountpoint,
        &options,
    )
        .unwrap();
    debug!("Exiting...");

    Ok(())
}

pub async fn sample_fs() -> Result<()> {
    let mountpoint = "/tmp/fuse/1";
    let source = "/tmp/fuse/2";
    let options = vec![MountOption::RW];
    debug!("Mounting fuse filesystem at {}", mountpoint);
    let fs = SampleFilesystem::new(mountpoint, source);

    fuser::mount2(fs, mountpoint, &options).unwrap();

    debug!("Exiting...");
    Ok(())
}

pub async fn sample_drive_fs() -> Result<()> {
    let mountpoint = "/tmp/fuse/3";
    let upload_ignore_path = Path::new("config/.upload_ignore");
    let settings_path = Path::new("config/settings.json");

    let cache_dir = get_cache_dir()?;
    let upload_ignore = CommonFileFilter::from_path(upload_ignore_path)?;
    let sync_settings = SyncSettings::new(Duration::from_secs(2), Duration::from_secs(20));
    // let source = "/tmp/fuse/2";
    let drive = GoogleDrive::new().await?;
    // let file_uploader = FileUploader::new("config/credentials.json", "config/token.json");
    let (file_uploader_sender, file_uploader_receiver) = mpsc::channel(1);
    let mut file_uploader = DriveFileUploader::new(drive.clone(),
                                                   upload_ignore,
                                                   file_uploader_receiver,
                                                   cache_dir.path().to_path_buf(),
                                                   Duration::from_secs(3));
    debug!("Mounting fuse filesystem at {}", mountpoint);
    let fs = DriveFilesystem::new(mountpoint,
                                  Path::new(""),
                                  file_uploader_sender.clone(),
                                  drive,
                                  cache_dir.into_path(),
                                  sync_settings,
    ).await?;

    // let session_unmounter =
    let mount_options = vec![MountOption::RW];

    let uploader_handle: JoinHandle<()> = tokio::spawn(async move { file_uploader.listen().await; });
    let end_signal_handle: JoinHandle<()> = mount(fs, &mountpoint, &mount_options, file_uploader_sender).await?;
    tokio::try_join!(uploader_handle, end_signal_handle)?;

    // tokio::spawn(async move {
    // end_program_signal_awaiter(file_uploader_sender, session_unmounter).await?;
    // });
    // fuser::mount2(fs, &mountpoint, &options).unwrap();


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

async fn mount(fs: DriveFilesystem,
               mountpoint: &str,
               options: &[MountOption],
               sender: Sender<FileUploaderCommand>) -> Result<JoinHandle<()>> {
    let mut session = Session::new(fs, mountpoint.as_ref(), options)?;
    let session_ender = session.unmount_callable();
    let end_program_signal_handle = tokio::spawn(async move {
        end_program_signal_awaiter(sender, session_ender).await;
    });
    debug!("Mounting fuse filesystem" );
    session.run();
    debug!("Finished with mounting");
    // Ok(session_ender)
    Ok(end_program_signal_handle)
}

async fn end_program_signal_awaiter(file_uploader_sender: Sender<FileUploaderCommand>,
                                    mut session_unmounter: SessionUnmounter) -> Result<()> {
    tokio::signal::ctrl_c().await.expect("failed to listen for ctrl_c event");

    info!("got signal to end program");
    file_uploader_sender.send(FileUploaderCommand::Stop).await?;
    info!("sent stop command to file uploader");
    info!("unmounting...");
    session_unmounter.unmount()?;
    info!("unmounted");
    Ok(())
}

/*
// pub async fn watch_file_reading() -> Result<()> {
//     let temp_file = tempfile::NamedTempFile::new()?;
//     let file_path = temp_file.path();
//     info!("File path: {:?}", file_path);
//     use notify::{recommended_watcher, RecursiveMode, Watcher};
//     let mut config = notify::Config::default();
//     let mut watcher: INotifyWatcher = Watcher::new(MyReadHandler, config).unwrap();
//     watcher
//         .watch(file_path, RecursiveMode::NonRecursive)
//         .unwrap();
//
//     info!("Press any key to exit...");
//     let x = &mut [0u8; 1];
//     stdin().read(x).await?;
//     debug!("Done");
//     Ok(())
// }
// struct MyReadHandler;
// impl notify::EventHandler for MyReadHandler {
//     fn handle_event(&mut self, event: std::result::Result<notify::Event, notify::Error>) {
//         debug!("File read: {:?}", event);
//     }
// }
//
// pub async fn sample_nix() -> Result<()> {
//     info!("Hello, world! (nix)");
//     let tmppath = tempfile::tempdir()?;
//     nix::mount::mount(
//         // Some("/home/omgeeky/Documents/testmount/"),
//         None::<&str>,
//         tmppath.path(),
//         Some("tmpfs"),
//         nix::mount::MsFlags::empty(),
//         None::<&str>,
//     );
//     info!("Mounted tmpfs at {:?}", tmppath.path());
//     info!("Press any key to exit (nix)...");
//     // block execution until keyboard input is received
//     nix::unistd::read(0, &mut [0])?;
//     nix::mount::umount(tmppath.path()).unwrap();
//     info!("Done (nix)");
//     Ok(())
// }
*/
