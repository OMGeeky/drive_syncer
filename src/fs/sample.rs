// use crate::async_helper::run_async_in_sync;
use crate::async_helper::run_async_blocking;
use crate::common::LocalPath;
use crate::fs::common::CommonFilesystem;
use crate::fs::inode::Inode;
use crate::fs::CommonEntry;
use crate::prelude::*;
use fuser::{
    FileAttr, FileType, KernelConfig, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry,
    ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr, Request, TimeOrNow, FUSE_ROOT_ID,
};
use libc::c_int;
use log::{debug, warn};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Display;
use std::fs::OpenOptions;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct SampleEntry {
    pub ino: Inode,

    pub name: OsString,
    pub local_path: LocalPath,
    pub attr: FileAttr,
}

impl SampleEntry {
    // fn new(ino: impl Into<Inode>,  local_path: OsString, attr: FileAttr) -> Self {
    //     Self {
    //         ino: ino.into(),
    //         name: OsString::new(),
    //         local_path: LocalPath::from(Path::new(&local_path)),
    //         attr,
    //     }
    // }

    fn new(
        ino: impl Into<Inode>,
        name: impl Into<OsString>,
        local_path: impl Into<LocalPath>,
        attr: FileAttr,
    ) -> Self {
        Self {
            ino: ino.into(),
            name: name.into(),
            local_path: local_path.into(),
            attr,
        }
    }
}

impl CommonEntry for SampleEntry {
    fn get_ino(&self) -> Inode {
        self.ino
    }

    fn get_name(&self) -> &OsStr {
        self.name.as_os_str()
    }

    fn get_local_path(&self) -> &LocalPath {
        &self.local_path
    }

    fn get_attr(&self) -> &FileAttr {
        &self.attr
    }
}
#[derive(Debug, Default)]
pub struct SampleFilesystem {
    /// the point where the filesystem is mounted
    root: PathBuf,
    /// the source dir to read from and write to
    source: PathBuf,

    /// How long the responses can/should be cached
    time_to_live: Duration,

    entries: HashMap<Inode, SampleEntry>,

    children: HashMap<Inode, Vec<Inode>>,

    /// The generation of the filesystem
    /// This is used to invalidate the cache
    /// when the filesystem is remounted
    generation: u64,
}
impl SampleFilesystem {
    pub fn new(root: impl AsRef<Path>, source: impl AsRef<Path>) -> Self {
        debug!("new: {:?}; {:?}", root.as_ref(), source.as_ref());
        let mut entries = HashMap::new();
        // Add root directory with inode number 1
        let root_attr = FileAttr {
            ino: FUSE_ROOT_ID,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        };
        entries.insert(
            FUSE_ROOT_ID.into(),
            SampleEntry::new(
                FUSE_ROOT_ID,
                "root",
                LocalPath::from(root.as_ref()),
                root_attr,
            ),
        );

        Self {
            root: root.as_ref().to_path_buf(),
            source: source.as_ref().to_path_buf(),
            time_to_live: Duration::from_secs(2),
            entries,
            /*TODO: implement a way to increase this if necessary*/
            generation: 0,
            children: HashMap::new(),
        }
    }
}
#[async_trait::async_trait]
impl CommonFilesystem<SampleEntry> for SampleFilesystem {
    fn get_entries(&self) -> &HashMap<Inode, SampleEntry> {
        &self.entries
    }
    fn get_entries_mut(&mut self) -> &mut HashMap<Inode, SampleEntry> {
        &mut self.entries
    }
    fn get_children(&self) -> &HashMap<Inode, Vec<Inode>> {
        &self.children
    }
    fn get_children_mut(&mut self) -> &mut HashMap<Inode, Vec<Inode>> {
        &mut self.children
    }
    fn get_root_path(&self) -> LocalPath {
        self.source.clone().into()
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
        let ino = self.generate_ino(); // Generate a new inode number
        let now = std::time::SystemTime::now();
        let attr = FileAttr {
            ino: ino.into(),
            size: size,
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
            blksize: 4096,
            flags: 0,
        };

        self.get_entries_mut()
            .insert(ino, SampleEntry::new(ino, name, OsString::from(name), attr));

        self.add_child(parent_ino, &ino);
        Ok(ino)
    }
}
impl SampleFilesystem {
    async fn add_dir_entry(
        &mut self,
        folder_path: &Path,
        parent_ino: impl Into<Inode>,
        skip_self: bool,
    ) -> Result<()> {
        let parent_ino = parent_ino.into();
        let ino: Inode;
        if skip_self {
            ino = parent_ino;
        } else {
            ino = self
                .add_entry(
                    folder_path.file_name().unwrap(),
                    /*TODO: correct permissions*/
                    0o755,
                    FileType::Directory,
                    parent_ino,
                    /*TODO: implement size for folders*/ 0,
                )
                .await?;
        }
        let d = std::fs::read_dir(folder_path);
        if let Ok(d) = d {
            for entry in d {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    let name = entry.file_name();
                    let metadata = entry.metadata();
                    if let Ok(metadata) = metadata {
                        if metadata.is_dir() {
                            self.add_dir_entry(&path, ino, false);
                        } else if metadata.is_file() {
                            let mode = metadata.mode();
                            let size = metadata.size();
                            //TODO: async call
                            // self.add_file_entry(ino, name.as_os_str(), mode as u16, size);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl fuser::Filesystem for SampleFilesystem {
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut KernelConfig,
    ) -> std::result::Result<(), c_int> {
        debug!("init");
        // self.add_file_entry(1, "hello.txt".as_ref(), 0o644);
        let source = self.source.clone();

        run_async_blocking(async {
            self.add_dir_entry(&source, FUSE_ROOT_ID, true).await;
        });
        // self.add_dir_entry(&source, FUSE_ROOT_ID, true);
        Ok(())
    }
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup: {}:{:?}", parent, name);
        for (inode, entry) in self.entries.iter() {
            let path: PathBuf = entry.local_path.clone().into();
            let accepted = name.eq_ignore_ascii_case(&path);
            debug!(
                "entry: {}:(accepted={}){:?}; {:?}",
                inode, accepted, path, entry.attr
            );
            if accepted {
                reply.entry(&self.time_to_live, &entry.attr, self.generation);
                return;
            }
        }

        reply.error(libc::ENOENT);
    }
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        self.entries.get(&ino.into()).map(|entry| {
            reply.attr(&self.time_to_live, &entry.attr);
        });
    }
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
        if !(children.is_none()) {
        } else {
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
            offset += 1; // Increment the offset for each processed entry
            debug!("entry: {}:{:?}; {:?}", inode, path, attr);
            if !reply.add(inode, offset, attr.kind, path) {
                break;
            }
        }
        debug!("readdir: ok");
        reply.ok();
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
        debug!(
            "read: {}:{}:{}:{}:{:#x?}:{:?}",
            ino, fh, offset, size, flags, lock_owner
        );
        let data = self.get_entry(ino).map(|entry| entry.attr);
        if let Some(attr) = data {
            if attr.kind != FileType::RegularFile {
                reply.error(libc::EISDIR);
                return;
            }

            let path = self.get_full_path_from_ino(ino);
            debug!("opening file: {:?}", &path);
            let mut file = std::fs::File::open::<PathBuf>(path.clone().unwrap().into()).unwrap();
            let mut buf = vec![0; size as usize];
            debug!("reading file: {:?} at {} with size {}", &path, offset, size);
            file.read_at(&mut buf, offset as u64).unwrap();
            debug!("read file: {:?} at {}", &path, offset);
            reply.data(&buf);
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
        debug!(
            "write: {}:{}:{}:{:#x?}:{:?}:{:#x?}:{:?}",
            ino, fh, offset, flags, lock_owner, write_flags, data,
        );
        let attr = self.get_entry(ino).map(|entry| entry.attr);
        if let Some(attr) = attr {
            if attr.kind != FileType::RegularFile {
                warn!(
                    "write: not a file, writing is not supported: kind:{:?}; attr:{:?}",
                    attr.kind, attr
                );
                reply.error(libc::EISDIR);
                return;
            }

            let path = self.get_full_path_from_ino(ino);
            debug!("opening file: {:?}", &path);
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .open::<PathBuf>(path.clone().unwrap().into())
                .unwrap();
            debug!(
                "writing file: {:?} at {} with size {}",
                &path,
                offset,
                data.len()
            );

            let size = file.write_at(data, offset as u64).unwrap();
            debug!("wrote file: {:?} at {}; wrote {} bits", &path, offset, size);
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
        let attr = self
            .entries
            .get_mut(&ino.into())
            .map(|entry| &mut entry.attr);
        if attr.is_none() {
            reply.error(libc::ENOENT);
            return;
        }
        let mut attr = attr.unwrap();

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
        if let Some(atime) = _atime {
            attr.atime = Self::convert_to_system_time(atime);
        }
        if let Some(mtime) = _mtime {
            attr.mtime = Self::convert_to_system_time(mtime);
        }
        if let Some(ctime) = _ctime {
            attr.ctime = ctime;
        }
        if let Some(crtime) = _crtime {
            attr.crtime = crtime;
        }
        if let Some(flags) = flags {
            attr.flags = flags;
        }

        reply.attr(&self.time_to_live, attr);
    }
    fn access(&mut self, _req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        reply.ok(); //TODO: implement this a bit better/more useful
    }
}
