use std::{
    any::Any,
    collections::HashMap,
    ffi::{OsStr, OsString},
    fmt::Display,
    fs::OpenOptions,
    os::unix::prelude::*,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::io::{Seek, SeekFrom, Write};
use std::ops::Deref;

use anyhow::{anyhow, Context, Error};
use async_recursion::async_recursion;
use drive3::api::File;
use fuser::{
    FileAttr,
    Filesystem,
    FileType,
    FUSE_ROOT_ID,
    KernelConfig,
    ReplyAttr,
    ReplyData,
    ReplyDirectory,
    ReplyEmpty,
    ReplyEntry,
    ReplyIoctl,
    ReplyLock,
    ReplyLseek,
    ReplyOpen,
    ReplyStatfs,
    ReplyWrite,
    ReplyXattr,
    Request,
    TimeOrNow,
};
use futures::TryFutureExt;
use libc::c_int;
use mime::Mime;
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, stdin},
    runtime::Runtime,
};
use tracing::{debug, error, instrument, warn};

use crate::{async_helper::run_async_blocking, common::LocalPath, config::common_file_filter::CommonFileFilter, fs::common::CommonFilesystem, fs::CommonEntry, fs::drive::DriveEntry, fs::inode::Inode, google_drive::{DriveId, GoogleDrive}, google_drive, prelude::*};
use crate::fs::drive::{FileCommand, FileUploaderCommand, SyncSettings};

enum CacheState {
    Missing,
    UpToDate,
    RefreshNeeded,
}

#[derive(Debug)]
pub struct DriveFilesystem {
    // runtime: Runtime,
    /// the point where the filesystem is mounted
    root: PathBuf,
    /// the source dir to read from and write to
    source: GoogleDrive,
    /// the cache dir to store the files in
    cache_dir: Option<PathBuf>,

    /// the filter to apply when uploading files
    // upload_filter: CommonFileFilter,

    entries: HashMap<Inode, DriveEntry>,

    children: HashMap<Inode, Vec<Inode>>,

    /// with this we can send a path to the file uploader
    /// to tell it to upload certain files.
    file_uploader_sender: tokio::sync::mpsc::Sender<FileUploaderCommand>,

    /// The generation of the filesystem
    /// This is used to invalidate the cache
    /// when the filesystem is remounted
    generation: u64,

    settings: SyncSettings,
}

impl Display for DriveFilesystem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DriveFilesystem {{ entries: {}, gen: {}, settings: {} }}",
               // self.root.display(),
               // self.cache_dir.as_ref().map(|dir| dir.path().display()),
               self.entries.len(),
               self.generation,
               self.settings,
        )
    }
}

impl DriveFilesystem {
    #[instrument(fields(% self, entry))]
    async fn schedule_upload(&self, entry: &DriveEntry) -> Result<()> {
        debug!("DriveFilesystem::schedule_upload(entry: {:?})", entry);
        let path = self.get_cache_path_for_entry(entry)?;
        let metadata = Self::create_drive_metadata_from_entry(entry)?;
        debug!("schedule_upload: sending path to file uploader...");
        self.file_uploader_sender.send(FileUploaderCommand::UploadChange(FileCommand::new(path, metadata))).await?;
        debug!("schedule_upload: sent path to file uploader");
        Ok(())
    }

    fn create_drive_metadata_from_entry(entry: &DriveEntry) -> Result<File> {
        Ok(File {
            drive_id: match entry.drive_id.clone().into_string() {
                Ok(v) => Some(v),
                Err(_) => None
            },
            // name: match entry.name.clone().into_string() {
            //     Ok(v) => Some(v),
            //     Err(_) => None
            // },
            // size: Some(entry.attr.size as i64),
            // modified_time: Some(entry.attr.mtime.into()),
            // file_extension: match entry.local_path.extension().clone() {
            //     Some(v) => v.to_str().map(|v| v.to_string()),
            //     None => None
            // },
            ..Default::default()
        })
    }
}

// region general
impl DriveFilesystem {
    #[instrument(skip(file_uploader_sender))]
    pub async fn new(root: impl AsRef<Path> + Debug,
                     config_path: impl AsRef<Path> + Debug,
                     file_uploader_sender: tokio::sync::mpsc::Sender<FileUploaderCommand>,
                     drive: GoogleDrive, cache_dir: PathBuf,
                     settings: SyncSettings) -> Result<DriveFilesystem> {
        let root = root.as_ref();
        let config_path = config_path.as_ref();
        debug!("DriveFilesystem::new(root:{}; config_path: {})", root.display(), config_path.display());
        // let upload_filter = CommonFileFilter::from_path(config_path)?;
        let mut entries = HashMap::new();
        let now = SystemTime::now();
        // Add root directory with inode number 1
        let root_attr = FileAttr {
            ino: FUSE_ROOT_ID,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        };
        let inode = Inode::from(FUSE_ROOT_ID);
        entries.insert(
            inode,
            DriveEntry::new(
                inode,
                "root".to_string(),
                DriveId::root(),
                LocalPath::from(Path::new("")),
                root_attr,
                None,
            ),
        );

        let mut s = Self {
            root: root.to_path_buf(),
            source: drive,
            cache_dir: Some(cache_dir),
            entries,
            file_uploader_sender,
            /*TODO: implement a way to increase this if necessary*/
            generation: 0,
            children: HashMap::new(),
            settings,
        };
        //
        // let root = s.root.to_path_buf();
        // s.add_dir_entry(&root, Inode::from(FUSE_ROOT_ID), true)
        //     .await;

        Ok(s)
    }
    #[instrument(fields(% self, inode))]
    fn get_cache_dir_for_file(&self, inode: Inode) -> Result<PathBuf> {
        debug!("get_cache_dir_for_file: {}", inode);
        let cache_dir = self.cache_dir.as_ref().ok_or(anyhow!("no cache dir"))?;
        debug!(
            "get_cache_dir_for_file: {}, cache_dir: {}",
            inode,
            cache_dir.display()
        );
        let entry = self
            .get_entry(inode)
            .ok_or(anyhow!("could not get entry"))?;
        debug!(
            "get_cache_dir_for_file: entry local_path: {}",
            entry.local_path.display()
        );
        let path = Self::construct_cache_folder_path(cache_dir, entry);
        debug!("get_cache_dir_for_file: {}: {}", inode, path.display());
        Ok(path)
    }

    #[instrument]
    fn construct_cache_folder_path(cache_dir: &Path, entry: &DriveEntry) -> PathBuf {
        let folder_path = match entry.local_path.parent() {
            Some(p) => p.as_os_str(),
            None => OsStr::new(""),
        };
        debug!("construct_cache_folder_path: folder_path: {:?}", folder_path);
        let path = cache_dir.join(folder_path);
        debug!("construct_cache_folder_path: {}", path.display());
        path
    }
    #[async_recursion::async_recursion]
    #[instrument(fields(% self, folder_path, parent_ino, inline_self))]
    async fn add_dir_entry(
        &mut self,
        folder_path: &Path,
        parent_ino: Inode,
        inline_self: bool,
    ) -> Result<()> {
        let ino;
        debug!(
            "add_dir_entry: {:?}; parent: {}; inline_self: {} ",
            folder_path, parent_ino, inline_self
        );
        if self.root == folder_path {
            debug!("add_dir_entry: root folder");
            ino = parent_ino;
        } else if inline_self {
            debug!("add_dir_entry: inlining self entry for {:?}", folder_path);
            ino = parent_ino;
        } else {
            debug!("add_dir_entry: adding entry for {:?}", folder_path);
            ino = self
                .add_entry(
                    folder_path.file_name().ok_or(anyhow!("invalid filename"))?,
                    /*TODO: correct permissions*/
                    0o755,
                    FileType::Directory,
                    parent_ino,
                    /*TODO: implement size for folders*/ 0,
                )
                .await?;
        }

        let drive = &self.source;

        let folder_drive_id: DriveId = self
            .get_drive_id(ino)
            .ok_or(anyhow!("could not find dir drive_id"))?;
        debug!(
            "add_dir_entry: getting files for '{:50?}'  {}",
            folder_drive_id,
            folder_path.display()
        );
        let files;
        {
            let files_res = self.source.list_files(folder_drive_id).await;
            if let Err(e) = files_res {
                warn!("could not get files: {}", e);
                return Ok(());
            }
            files = files_res.unwrap();
        }
        debug!("got {} files", files.len());
        // let d = std::fs::read_dir(folder_path);

        for entry in files {
            debug!("entry: {:?}", entry);
            let name = entry.name.as_ref().ok_or_else(|| "no name");
            if let Err(e) = name {
                warn!("could not get name: {}", e);
                continue;
            }
            let name = name.as_ref().unwrap();
            if name.contains("/") || name.contains("\\") || name.contains(":") {
                warn!("invalid name: {}", name);
                continue;
            }
            let path = folder_path.join(&name);

            if let None = &entry.mime_type {
                warn!("could not get mime_type");
                continue;
            }

            let mime_type = entry.mime_type.as_ref().unwrap();
            if mime_type == "application/vnd.google-apps.document"
                || mime_type == "application/vnd.google-apps.spreadsheet"
                || mime_type == "application/vnd.google-apps.drawing"
                || mime_type == "application/vnd.google-apps.form"
                || mime_type == "application/vnd.google-apps.presentation"
                || mime_type == "application/vnd.google-apps.drive-sdk"
                || mime_type == "application/vnd.google-apps.script"
            //TODO: add all relevant mime types
            {
                debug!(
                    "skipping google file: mime_type: '{}' entry: {:?}",
                    mime_type, entry
                );
                continue;
            } else if mime_type == "application/vnd.google-apps.folder" {
                debug!("adding folder: {:?}", path);
                let res = self.add_dir_entry(&path, ino, false).await;
                if let Err(e) = res {
                    warn!("could not add folder: {}", e);
                    continue;
                }
            } else {
                debug!("adding file: '{}' {:?}", mime_type, path);
                let size = match Self::get_size_from_drive_metadata(&entry) {
                    Some(value) => value,
                    None => continue,
                };
                let mode = 0o644; //TODO: get mode from settings

                self.add_file_entry(ino, &OsString::from(&name), mode as u16, size)
                    .await;
            }
        }

        Ok(())
    }

    #[instrument]
    fn get_size_from_drive_metadata(entry: &File) -> Option<u64> {
        let size = entry.size.ok_or_else(|| 0);
        if let Err(e) = size {
            warn!("could not get size: {}", e);
            return None;
        }
        let size = size.unwrap();
        if size < 0 {
            warn!("invalid size: {}", size);
            return None;
        }
        let size = size as u64;
        Some(size)
    }
    #[instrument(fields(% self, ino))]
    fn get_drive_id(&self, ino: impl Into<Inode>) -> Option<DriveId> {
        self.get_entry(ino).map(|e| e.drive_id.clone())
    }
}
// endregion

// region caching
impl DriveFilesystem {
    async fn download_file_to_cache(&mut self, ino: impl Into<Inode>) -> Result<PathBuf> {
        let ino = ino.into();
        debug!("download_file_to_cache: {}", ino);
        let entry = self.get_entry_r(ino)?;
        let drive_id = entry.drive_id.clone();
        let drive = &self.source;
        let cache_path = self.get_cache_path_for_entry(&entry)?;
        let folder = cache_path.parent()
                               .ok_or(anyhow!("could not get the folder the cache file should be saved in"))?;
        if !folder.exists() {
            debug!("creating folder: {}", folder.display());
            std::fs::create_dir_all(folder)?;
        }
        debug!("downloading file: {}", cache_path.display());
        drive.download_file(drive_id, &cache_path).await?;
        debug!("downloaded file: {}", cache_path.display());
        let size = std::fs::metadata(&cache_path)?.len();
        self.set_entry_size(ino, size);
        self.set_entry_cached(ino)?;
        //TODO: check if any other things need to be updated for the entry
        Ok(cache_path)
    }

    fn set_entry_cached(&mut self, ino: Inode) -> Result<()> {
        let mut entry = self.get_entry_mut(ino).ok_or(anyhow!("could not get entry"))?;
        entry.content_cache_time = Some(SystemTime::now());
        Ok(())
    }
    fn check_if_file_is_cached(&self, ino: impl Into<Inode> + Debug) -> Result<bool> {
        let entry = self.get_entry_r(ino)?;
        let path = self.get_cache_path_for_entry(&entry)?;
        let exists = path.exists();
        Ok(exists)
    }
    #[instrument(fields(% self))]
    async fn update_entry_metadata_cache_if_needed(&mut self, ino: impl Into<Inode> + Debug) -> Result<()> {
        //TODO: do something that uses the changes api so not every file needs to check for 
        // itself if it needs to update, rather it gets checked once and then updates all the 
        // cache times for all files
        
        let ino = ino.into();
        let entry = self.get_entry_r(ino)?;
        let refresh_cache = self.get_cache_state(&entry.metadata_cache_time);
        match refresh_cache {
            CacheState::RefreshNeeded | CacheState::Missing => {
                debug!("refreshing metadata cache for drive_id: {:?}", entry.drive_id);
                let metadata = self.source.get_metadata_for_file(entry.drive_id.clone()).await?;
                self.update_entry_metadata(ino, &metadata)?;
                self.set_entry_metadata_cached(ino)?;
            }
            CacheState::UpToDate => {
                debug!("metadata cache is up to date");
            }
            _ => {
                debug!("unknown cache state");
            }
        }
        Ok(())
    }
    #[instrument(fields(% self))]
    async fn update_cache_if_needed(&mut self, ino: impl Into<Inode> + Debug) -> Result<()> {
        let ino = ino.into();
        self.update_entry_metadata_cache_if_needed(ino).await?;
        let entry = match self.get_entry_r(ino) {
            Ok(entry) => entry,
            Err(e) => {
                warn!("could not get entry: {}", e);
                return Err(e);
            }
        };
        let refresh_cache = self.get_cache_state(&entry.content_cache_time);
        match refresh_cache {
            CacheState::Missing => {
                debug!("no local cache for: {}, downloading...", ino);
                self.download_file_to_cache(ino).await?;
            }
            CacheState::RefreshNeeded => {
                debug!("cache needs refresh for: {}, checking for updated version...", ino);
                let remote_mod_time: SystemTime = self.get_modified_time_on_remote(ino).await?;
                debug!("remote_mod_time: {:?}", remote_mod_time);
                let local_mod_time = self.get_entry_r(ino)?.attr.mtime;
                debug!("local_mod_time: {:?}", local_mod_time);
                if remote_mod_time > local_mod_time {
                    debug!("updating cached file since remote_mod_time: {:?} > local_mod_time: {:?}", remote_mod_time, local_mod_time);
                    self.download_file_to_cache(ino).await?;
                } else {
                    debug!("local file is up to date: remote_mod_time: {:?} <= local_mod_time: {:?}", remote_mod_time, local_mod_time);
                }
            }
            CacheState::UpToDate => {
                debug!("Cache up to date for {} since {:?} > {}", ino, entry.content_cache_time.unwrap(), self.settings.cache_time().as_secs());
            }
        }
        Ok(())
    }

    fn get_cache_state(&self, cache_time: &Option<SystemTime>) -> CacheState {
        let refresh_cache: CacheState = match cache_time {
            Some(cache_time) => {
                let now = SystemTime::now();
                let duration = now.duration_since(*cache_time).unwrap();
                // let seconds = duration.as_secs();
                if duration > self.settings.cache_time() {
                    CacheState::RefreshNeeded
                } else {
                    CacheState::UpToDate
                }
            }
            None => CacheState::Missing,
        };
        refresh_cache
    }

    fn get_cache_path_for_entry(&self, entry: &DriveEntry) -> Result<PathBuf> {
        debug!("get_cache_path_for_entry: {}", entry.ino);
        let cache_folder = match self.cache_dir.as_ref() {
            Some(x) => x,
            None => return Err(anyhow!("cache_dir is None").into()),
        };
        let path = Self::construct_cache_path_for_entry(&cache_folder, entry);
        Ok(path)
    }
    fn construct_cache_path_for_entry(cache_dir: &Path, entry: &DriveEntry) -> PathBuf {
        debug!("construct_cache_path_for_entry: {} with cache_dir: {}", entry.ino, cache_dir.display());
        let path = Self::construct_cache_folder_path(cache_dir, entry).join(&entry.name);
        debug!(
            "get_cache_path_for_entry: {}: {}",
            entry.ino,
            path.display()
        );
        path
    }
    async fn get_modified_time_on_remote(&self, ino: Inode) -> Result<SystemTime> {
        let entry = self.get_entry_r(ino)?;
        let drive_id = entry.drive_id.clone();
        let drive = &self.source;
        let modified_time = drive.get_modified_time(drive_id).await?;
        Ok(modified_time)
    }
    fn set_entry_size(&mut self, ino: Inode, size: u64) -> anyhow::Result<()> {
        self.get_entry_mut(ino).context("no entry for ino")?.attr.size = size;
        Ok(())
    }
    fn update_entry_metadata(&mut self, ino: Inode, drive_metadata: &google_drive3::api::File) -> anyhow::Result<()> {
        let entry = self.get_entry_mut(ino).context("no entry with ino")?;
        if let Some(name) = drive_metadata.name.as_ref() {
            entry.name = OsString::from(name);
        }
        if let Some(size) = drive_metadata.size.as_ref() {
            entry.attr.size = *size as u64;
        }
        if let Some(modified_time) = drive_metadata.modified_time.as_ref() {
            entry.attr.mtime = (*modified_time).into();
        }
        if let Some(created_time) = drive_metadata.created_time.as_ref() {
            entry.attr.ctime = (*created_time).into();
        }
        if let Some(viewed_by_me) = drive_metadata.viewed_by_me_time.as_ref(){
            entry.attr.atime = (*viewed_by_me).into();
        }

        Ok(())
    }
    fn set_entry_metadata_cached(&mut self, ino: Inode) -> anyhow::Result<()> {
        let mut entry = self.get_entry_mut(ino).context("no entry with ino")?;
        entry.metadata_cache_time = Some(SystemTime::now());
        Ok(())
    }
}

// endregion

// region common
#[async_trait::async_trait]
impl CommonFilesystem<DriveEntry> for DriveFilesystem {
    fn get_entries(&self) -> &HashMap<Inode, DriveEntry> {
        &self.entries
    }

    fn get_entries_mut(&mut self) -> &mut HashMap<Inode, DriveEntry> {
        &mut self.entries
    }

    fn get_children(&self) -> &HashMap<Inode, Vec<Inode>> {
        &self.children
    }

    fn get_children_mut(&mut self) -> &mut HashMap<Inode, Vec<Inode>> {
        &mut self.children
    }

    fn get_root_path(&self) -> LocalPath {
        self.root.clone().into()
    }

    #[instrument(fields(% self, name, mode, file_type, parent_ino, size))]
    async fn add_entry(
        &mut self,
        name: &OsStr,
        mode: u16,
        file_type: FileType,
        parent_ino: impl Into<Inode> + Send + Debug,
        size: u64,
    ) -> Result<Inode> {
        let parent_ino = parent_ino.into();
        debug!("add_entry: (0) name:{:20?}; parent: {}", name, parent_ino);
        let ino = self.generate_ino(); // Generate a new inode number
        let now = std::time::SystemTime::now();
        //TODO: write the actual creation and modification time, not just now
        let attr = FileAttr {
            ino: ino.into(),
            size: size,
            /* TODO: set block size to something usefull.
            maybe set it to 0 but when the file is cached set it to however big the
            file in the cache is? that way it shows the actual size in blocks that are
            used*/
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: file_type,
            perm: mode,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            /*TODO: set the actual block size?*/
            blksize: 4096,
            flags: 0,
        };

        let parent_drive_id = self.get_drive_id(parent_ino);
        let drive_id: DriveId = self.source.get_id(name, parent_drive_id).await?;
        debug!("add_entry: (1) drive_id: {:?}", drive_id);

        let parent_local_path = self.get_path_from_ino(parent_ino);
        let parent_path: PathBuf = parent_local_path
            .ok_or(anyhow!("could not get local path"))?
            .into();

        self.get_entries_mut().insert(
            ino,
            DriveEntry::new(ino, name, drive_id, parent_path.join(name), attr, None),
        );

        self.add_child(parent_ino, &ino);
        debug!("add_entry: (2) after adding count: {}", self.entries.len());
        Ok(ino)
    }
}

// endregion

//region some convenience functions/implementations
// fn check_if_entry_is_cached

//endregion

//region filesystem
impl Filesystem for DriveFilesystem {
    //region init
    #[instrument(skip(_req, _config), fields(% self))]
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut KernelConfig,
    ) -> std::result::Result<(), c_int> {
        debug!("init");

        let root = self.root.to_path_buf();
        let x = run_async_blocking(self.add_dir_entry(&root, Inode::from(FUSE_ROOT_ID), true));
        if let Err(e) = x {
            error!("could not add root entry: {}", e);
        }
        Ok(())
    }
    //endregion
    //region destroy
    #[instrument(fields(% self))]
    fn destroy(&mut self) {
        debug!("destroy");
        self.file_uploader_sender.send(FileUploaderCommand::Stop);
    }
    //endregion
    //region lookup
    #[instrument(skip(_req, reply), fields(% self))]
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup: {}:{:?}", parent, name);
        let parent = parent.into();
        let children = self.children.get(&parent);
        if children.is_none() {
            warn!("lookup: could not find children for {}", parent);
            reply.error(libc::ENOENT);
            return;
        }
        let children = children.unwrap().clone();
        debug!("lookup: children: {:?}", children);
        for child_inode in children {

            run_async_blocking(self.update_entry_metadata_cache_if_needed(child_inode));
            let entry = self.entries.get(&child_inode);
            if entry.is_none() {
                warn!("lookup: could not find entry for {}", child_inode);
                continue;
            }
            let entry = entry.unwrap();

            let path: PathBuf = entry.name.clone().into();
            let accepted = name.eq_ignore_ascii_case(&path);
            debug!(
                "entry: {}:(accepted={}){:?}; {:?}",
                child_inode, accepted, path, entry.attr
            );
            if accepted {
                reply.entry(&self.settings.time_to_live(), &entry.attr, self.generation);
                return;
            }
        }
        warn!("lookup: could not find entry for {:?}", name);

        reply.error(libc::ENOENT);
    }
    //endregion
    //region getattr
    #[instrument(skip(_req, reply), fields(% self))]
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        debug!("getattr: {}", ino);
        run_async_blocking(self.update_entry_metadata_cache_if_needed(ino));
        let entry = self.entries.get(&ino.into());
        if let Some(entry) = entry {
            reply.attr(&self.settings.time_to_live(), &entry.attr);
        } else {
            reply.error(libc::ENOENT);
        }
    }
    //endregion
    //region setattr
    #[instrument(skip(_req, reply), fields(% self))]
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
        /*TODO: check if this change need to be implemented*/
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        /*TODO: check if this change need to be implemented*/
        _chgtime: Option<SystemTime>,
        /*TODO: check if this change need to be implemented*/
        _bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        // debug!("setattr: {}", ino);

        debug!(
            "setattr: {}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}",
            ino,
            mode,
            uid,
            gid,
            size,
            _atime,
            _mtime,
            _ctime,
            fh,
            _crtime,
            _chgtime,
            _bkuptime,
            flags
        );
        let ttl = self.settings.time_to_live();
        let entry = self.get_entry_mut(ino);
        if let None = entry {
            error!("setattr: could not find entry for {}", ino);
            reply.error(libc::ENOENT);
            return;
        }
        let mut entry = entry.unwrap();
        let attr = &mut entry.attr;

        if let Some(mode) = mode {
            attr.perm = mode as u16;
        }
        if let Some(uid) = uid {
            attr.uid = uid;
        }
        if let Some(gid) = gid {
            attr.gid = gid;
        }
        if let Some(size) = size {
            attr.size = size;
        }
        if let Some(flags) = flags {
            attr.flags = flags;
        }
        reply.attr(&ttl, &attr);
        //TODO: update file on drive if necessary
    }
    //endregion
    //region write
    #[instrument(skip(_req, reply), fields(% self, data = data.len()))]
    fn write(&mut self,
             _req: &Request<'_>,
             ino: u64,
             fh: u64,
             offset: i64,
             data: &[u8],
             write_flags: u32,
             flags: i32,
             lock_owner: Option<u64>,
             reply: ReplyWrite) {
        debug!(
            "write: {}:{}:{}:{:#x?}:{:?}:{:#x?}:{:?}",
            ino, fh, offset, flags, lock_owner, write_flags, data,
        );
        let cache_update_success: Result<()> = run_async_blocking(self.update_cache_if_needed(ino));
        if let Err(e) = cache_update_success {
            error!("write: could not update cache: {}", e);
            reply.error(libc::EIO);
            return;
        }
        let cache_dir = self.cache_dir
                            .as_ref()
                            .map(|s| s.to_path_buf());
        if let None = cache_dir {
            error!("write: cache dir not set");
            reply.error(libc::ENOENT);
            return;
        }
        let cache_dir = cache_dir.unwrap();
        {
            let entry = self.get_entry_mut(ino);
            if let None = entry {
                error!("write: could not find entry for {}", ino);
                reply.error(libc::ENOENT);
                return;
            }
            let mut entry = entry.unwrap();
            //TODO: queue uploads on a separate thread

            let path = Self::construct_cache_path_for_entry(&cache_dir, &entry);
            // let path = entry.local_path.to_path_buf();
            debug!("opening file: {:?}", &path);
            let file = OpenOptions::new()
                .truncate(false)
                .create(true)
                .write(true)
                .open(&path);
            if let Err(e) = file {
                error!("write: could not open file: {:?}: {}", path, e);
                reply.error(libc::ENOENT);
                return;
            }
            let mut file = file.unwrap();

            debug!("writing file: {:?} at {} with  size {}",
                &path,
                offset,
                data.len()
        );

            file.seek(SeekFrom::Start(offset as u64)).unwrap();
            file.write_all(data).unwrap();
            let size = data.len();
            // let size = file.write_at(data, offset as u64);
            // if let Err(e) = size {
            //     error!("write: could not write file: {:?}: {}", path, e);
            //     reply.error(libc::ENOENT);
            //     return;
            // }
            // let size = size.unwrap();
            debug!("wrote   file: {:?} at {}; wrote {} bits", &path, offset, size);
            reply.written(size as u32);
            //TODO: update size in entry if necessary
            debug!("updating size to {} for entry: {:?}", entry.attr.size, entry);
            let mut attr = &mut entry.attr;
            attr.size = attr.size.max(offset as u64 + size as u64);
            let now = SystemTime::now();
            attr.mtime = now;
            attr.ctime = now;
            debug!("updated  size to {} for entry: {:?}", entry.attr.size, entry);
            debug!("write done for entry: {:?}", entry);
        }
        let entry = self.get_entry_r(&ino.into())
                        .expect("how could this happen to me. I swear it was there a second ago");
        run_async_blocking(self.schedule_upload(&entry));
    }
    //endregion
    //region read
    #[instrument(skip(_req, reply), fields(% self))]
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
        debug!(
            "read: {:10}:{:2}:{:3}:{:10}:{:10X}:{:?}",
            ino, fh, offset, size, flags, lock_owner
        );

        run_async_blocking(self.update_entry_metadata_cache_if_needed(ino));
        let x: Result<()> = run_async_blocking(self.update_cache_if_needed(ino));
        if let Err(e) = x {
            error!("read: could not update cache: {}", e);
            reply.error(libc::EIO);
            return;
        }
        // let is_cached = self.check_if_file_is_cached(ino);
        // if !is_cached.unwrap_or(false) {
        //     debug!("read: file is not cached: {}", ino);
        //     let x: Result<PathBuf> = run_async_blocking(self.download_file_to_cache(ino));
        //
        //     if let Err(e) = x {
        //         error!("read: could not download file: {}", e);
        //         reply.error(libc::ENOENT);
        //         return;
        //     }
        // }

        let entry = self.get_entry_r(&ino.into());
        if let Err(e) = entry {
            error!("read: could not find entry for {}: {}", ino, e);
            reply.error(libc::ENOENT);
            return;
        }
        let entry = entry.unwrap();

        let path = self.get_cache_path_for_entry(&entry);
        if let Err(e) = path {
            error!("read: could not get cache path: {}", e);
            reply.error(libc::ENOENT);
            return;
        }
        let path = path.unwrap();

        debug!("read: path: {:?}", path);
        let file = std::fs::File::open(&path);
        if let Err(e) = file {
            error!("read: could not open file: {}", e);
            reply.error(libc::EIO);
            return;
        }
        let mut file = file.unwrap();

        let mut buf = vec![0; size as usize];
        debug!("reading file: {:?} at {} with size {}", &path, offset, size);
        file.read_at(&mut buf, offset as u64).unwrap();
        debug!("read file: {:?} at {}", &path, offset);
        reply.data(&buf);
    }
    //endregion
    //region readdir
    #[instrument(skip(_req, reply), fields(% self, ino, fh, offset))]
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        mut offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir: {}:{}:{:?}", ino, fh, offset);
        run_async_blocking(self.update_entry_metadata_cache_if_needed(ino));
        let children = self.children.get(&ino.into());
        if let Some(attr) = self.get_entries().get(&ino.into()).map(|entry| entry.attr) {
            if attr.kind != FileType::Directory {
                reply.error(libc::ENOTDIR);
                return;
            }
        }
        if children.is_none() {
            reply.error(libc::ENOENT);
            return;
        }

        let children = children.unwrap();
        debug!("children ({}): {:?}", children.len(), children);
        for child_inode in children.iter().skip(offset as usize) {
            let entry = self.entries.get(child_inode).unwrap();
            let path: PathBuf = entry.local_path.clone().into();
            let attr = entry.attr;
            let inode = (*child_inode).into();
            // Increment the offset for each processed entry
            offset += 1;
            debug!("entry: {}:{:?}; {:?}", inode, path, attr);
            if reply.add(inode, offset, attr.kind, &entry.name) {
                // If the buffer is full, we need to stop
                debug!("readdir: buffer full");
                break;
            }
        }
        debug!("readdir: ok");
        reply.ok();
    }
    //endregion
    //region access
    #[instrument(fields(% self, ino, mask))]
    fn access(&mut self, _req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        reply.ok(); //TODO: implement this correctly
    }
    //endregion
}
//endregion
