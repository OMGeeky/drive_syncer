use crate::{
    google_drive::{DriveId, GoogleDrive},
    fs::CommonEntry,
    fs::inode::Inode,
    fs::common::CommonFilesystem,
    common::LocalPath,
    prelude::*,
    async_helper::run_async_blocking,
};
use anyhow::{anyhow, Error};
use async_recursion::async_recursion;
use drive3::api::File;
use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr, Request, TimeOrNow, FUSE_ROOT_ID,
};
use futures::TryFutureExt;
use libc::c_int;
use log::{debug, error, warn};
use mime::Mime;
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
use tempfile::TempDir;
use tokio::{
    io::{stdin, AsyncBufReadExt},
    runtime::Runtime,
};

pub use entry::*;

mod entry;

#[derive(Debug)]
pub struct DriveFilesystem {
    // runtime: Runtime,
    /// the point where the filesystem is mounted
    root: PathBuf,
    /// the source dir to read from and write to
    source: GoogleDrive,
    /// the cache dir to store the files in
    cache_dir: Option<TempDir>,

    /// How long the responses can/should be cached
    time_to_live: Duration,

    entries: HashMap<Inode, DriveEntry>,

    children: HashMap<Inode, Vec<Inode>>,

    /// The generation of the filesystem
    /// This is used to invalidate the cache
    /// when the filesystem is remounted
    generation: u64,
}

// region general
impl DriveFilesystem {
    pub async fn new(root: impl AsRef<Path>) -> Result<Self> {
        debug!("new: {:?};", root.as_ref());
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
        let inode = FUSE_ROOT_ID.into();
        entries.insert(
            inode,
            DriveEntry {
                ino: inode,
                name: "root".into(),
                local_path: LocalPath::from(Path::new("")),
                drive_id: DriveId::root(),
                // drive_path: "/".into(),
                attr: root_attr,
            },
        );

        let cache_dir = tempfile::tempdir()?;
        debug!("cache_dir: {:?}", cache_dir.path());
        if !cache_dir.path().exists() {
            debug!("creating cache dir: {:?}", cache_dir.path());
            std::fs::create_dir_all(cache_dir.path())?;
        } else {
            debug!("cache dir exists: {}", cache_dir.path().display());
        }
        let mut s = Self {
            root: root.as_ref().to_path_buf(),
            source: GoogleDrive::new().await?,
            cache_dir: Some(cache_dir),
            time_to_live: Duration::from_secs(2),
            entries,
            /*TODO: implement a way to increase this if necessary*/
            generation: 0,
            children: HashMap::new(),
        };
        //
        // let root = s.root.to_path_buf();
        // s.add_dir_entry(&root, Inode::from(FUSE_ROOT_ID), true)
        //     .await;

        Ok(s)
    }
    fn get_cache_dir_for_file(&self, inode: Inode) -> Result<PathBuf> {
        debug!("get_cache_dir_for_file: {}", inode);
        let cache_dir = self.cache_dir.as_ref().ok_or(anyhow!("no cache dir"))?;
        debug!(
            "get_cache_dir_for_file: {}, cache_dir: {}",
            inode,
            cache_dir.path().display()
        );
        let entry = self
            .entries
            .get(&inode)
            .ok_or(anyhow!("could not get entry"))?;
        debug!(
            "get_cache_dir_for_file: entry local_path: {}",
            entry.local_path.display()
        );
        let folder_path = match entry.local_path.parent() {
            Some(p) => p.as_os_str(),
            None => OsStr::new(""),
        };
        debug!("get_cache_dir_for_file: folder_path: {:?}", folder_path);
        let path = cache_dir.path().join(folder_path);
        debug!("get_cache_dir_for_file: {}: {}", inode, path.display());
        Ok(path)
    }
    #[async_recursion::async_recursion]
    async fn add_dir_entry(
        &mut self,
        folder_path: &Path,
        parent_ino: Inode,
        skip_self: bool,
    ) -> Result<()> {
        let ino;
        debug!(
            "add_dir_entry: {:?}; parent: {}; skip_self: {} ",
            folder_path, parent_ino, skip_self
        );
        if self.root == folder_path {
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
                //     } else if metadata.is_file() {
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
    fn get_drive_id(&self, ino: impl Into<Inode>) -> Option<DriveId> {
        self.get_entry(ino).map(|e| e.drive_id.clone())
    }
}
// endregion

// region caching
impl DriveFilesystem {
    async fn download_file_to_cache(&self, ino: impl Into<Inode>) -> Result<PathBuf> {
        let ino = ino.into();
        debug!("download_file_to_cache: {}", ino);
        let entry = self.get_entry_r(ino)?;
        let drive_id = entry.drive_id.clone();
        let drive = &self.source;
        let path = self.get_cache_path_for_entry(&entry)?;
        let folder = path.parent().unwrap();
        if !folder.exists() {
            debug!("creating folder: {}", folder.display());
            std::fs::create_dir_all(folder)?;
        }
        debug!("downloading file: {}", path.display());
        drive.download_file(drive_id, &path).await?;
        Ok(path)
    }
    fn check_if_file_is_cached(&self, ino: impl Into<Inode>) -> Result<bool> {
        let entry = self.get_entry_r(ino)?;
        let path = self.get_cache_path_for_entry(&entry)?;
        let exists = path.exists();
        Ok(exists)
    }

    fn get_cache_path_for_entry(&self, entry: &&DriveEntry) -> Result<PathBuf> {
        debug!("get_cache_path_for_entry: {}", entry.ino);
        let folder = self.get_cache_dir_for_file(entry.ino)?;
        let path = folder.join(&entry.name);
        debug!(
            "get_cache_path_for_entry: {}: {}",
            entry.ino,
            path.display()
        );
        Ok(path)
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

    async fn add_entry(
        &mut self,
        name: &OsStr,
        mode: u16,
        file_type: FileType,
        parent_ino: impl Into<Inode> + Send,
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
            DriveEntry::new(ino, name, drive_id, parent_path.join(name), attr),
        );

        self.add_child(parent_ino, &ino);
        debug!("add_entry: (2) after adding count: {}", self.entries.len());
        Ok(ino)
    }
}

// endregion

//region some convenience functions/implementations
impl Display for DriveFilesystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DriveFilesystem at: '/{}'", self.root.display())
    }
}

//endregion

//region filesystem
impl Filesystem for DriveFilesystem {
    //region init
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
    fn destroy(&mut self) {
        debug!("destroy");
        debug!("destroy: removing cache dir: {:?}", self.cache_dir);
        self.cache_dir = None;
    }
    //endregion
    //region lookup
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup: {}:{:?}", parent, name);
        let parent = parent.into();
        let children = self.children.get(&parent);
        if children.is_none() {
            warn!("lookup: could not find children for {}", parent);
            reply.error(libc::ENOENT);
            return;
        }
        let children = children.unwrap();
        debug!("lookup: children: {:?}", children);
        for child_inode in children {
            let entry = self.entries.get(child_inode);
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
                reply.entry(&self.time_to_live, &entry.attr, self.generation);
                return;
            }
        }
        warn!("lookup: could not find entry for {:?}", name);

        reply.error(libc::ENOENT);
    }
    //endregion
    //region getattr
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        debug!("getattr: {}", ino);
        let entry = self.entries.get(&ino.into());
        if let Some(entry) = entry {
            reply.attr(&self.time_to_live, &entry.attr);
        } else {
            reply.error(libc::ENOENT);
        }
    }
    //endregion
    //region read
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

        let entry = self.get_entry_r(&ino.into());
        if let Err(e) = entry {
            error!("read: could not find entry for {}: {}", ino, e);
            reply.error(libc::ENOENT);
            return;
        }
        let entry = entry.unwrap();

        let is_cached = self.check_if_file_is_cached(ino);
        if !is_cached.unwrap_or(false) {
            debug!("read: file is not cached: {}", ino);
            let x: Result<PathBuf> = run_async_blocking(self.download_file_to_cache(ino));

            if let Err(e) = x {
                error!("read: could not download file: {}", e);
                reply.error(libc::ENOENT);
                return;
            }
        }

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
            reply.error(libc::ENOENT);
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
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        mut offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir: {}:{}:{:?}", ino, fh, offset);
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
    fn access(&mut self, _req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        reply.ok(); //TODO: implement this correctly
    }
    //endregion
}
//endregion
