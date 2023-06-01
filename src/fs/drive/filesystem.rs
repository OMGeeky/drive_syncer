use std::fmt::{Debug, Formatter};
use std::io::{stdout, Seek, SeekFrom, Write};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fmt::Display,
    fs::OpenOptions,
    os::unix::prelude::*,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context};
use bimap::BiMap;
use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow,
    FUSE_ROOT_ID,
};
use google_drive3::api::{File, StartPageToken};
use libc::c_int;
use tracing::field::debug;
use tracing::{debug, error, instrument, warn};

use crate::fs::drive::{Change, ChangeType, FileCommand, FileUploaderCommand, SyncSettings};
use crate::{
    async_helper::run_async_blocking,
    common::LocalPath,
    fs::drive::DriveEntry,
    fs::inode::Inode,
    google_drive::{DriveId, GoogleDrive},
    prelude::*,
};

#[derive(Debug)]
enum ChecksumMatch {
    /// when the local, the cache and the remote checksum match
    Match,
    Unknown,
    Missing,
    /// when the cache does not match the remote or the local, but the remote and the local match
    ///
    /// this shows that some change has just been uploaded
    CacheMismatch,
    /// when the local does not match the remote or the cache, but the remote and the cache match
    ///
    /// this shows that the local file has been changed
    LocalMismatch,
    /// when the remote does not match the local or the cache, but the local and the cache match
    ///
    /// this shows that the remote file has been changed
    RemoteMismatch,
    /// when all three checksums are different
    ///
    /// this is used when the file has been changed locally and remotely
    ///
    /// this needs to be resolved manually
    Conflict,
}

#[derive(Debug)]
pub struct DriveFilesystem {
    /// the source dir to read from and write to
    source: GoogleDrive,
    /// the cache dir to store the files in
    cache_dir: Option<PathBuf>,

    entries: HashMap<DriveId, DriveEntry>,
    ino_drive_id: BiMap<Inode, DriveId>,
    children: HashMap<DriveId, Vec<DriveId>>,

    /// with this we can send a path to the file uploader
    /// to tell it to upload certain files.
    file_uploader_sender: tokio::sync::mpsc::Sender<FileUploaderCommand>,

    /// The generation of the filesystem
    /// This is used to invalidate the cache
    /// when the filesystem is remounted
    generation: u64,

    settings: SyncSettings,

    /// the token to use when requesting changes
    /// from the google drive api
    ///
    /// this should be initialized as soon as tracking starts
    /// and should be updated after retrieving every changelist
    changes_start_token: StartPageToken,

    /// the time when it was last checked for changes
    ///
    /// if this is longer ago than the configured duration
    /// the filesystem will check for changes with
    /// the changes_start_token on the google drive api
    last_checked_changes: SystemTime,
}

impl Display for DriveFilesystem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DriveFilesystem {{ entries: {} }}",
            // write!(f, "DriveFilesystem {{ entries: {}, gen: {}, settings: {} }}",
            // self.root.display(),
            // self.cache_dir.as_ref().map(|dir| dir.path().display()),
            self.entries.len(),
            // self.generation,
            // self.settings,
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
        self.file_uploader_sender
            .send(FileUploaderCommand::UploadChange(FileCommand::new(
                path, metadata,
            )))
            .await?;
        debug!("schedule_upload: sent path to file uploader");
        Ok(())
    }

    fn create_drive_metadata_from_entry(entry: &DriveEntry) -> Result<File> {
        Ok(File {
            drive_id: Some(entry.drive_id.clone().to_string()),
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

    fn get_drive_id_from_ino(&self, parent: impl Into<Inode>) -> anyhow::Result<&DriveId> {
        self.ino_drive_id
            .get_by_left(&parent.into())
            .context("could not get drive id for ino")
    }
    fn get_ino_from_drive_id(&self, parent: impl Into<DriveId>) -> anyhow::Result<&Inode> {
        self.ino_drive_id
            .get_by_right(&parent.into())
            .context("could not get drive id for ino")
    }
}

// region general
impl DriveFilesystem {
    #[instrument(skip(file_uploader_sender))]
    pub async fn new(
        config_path: impl AsRef<Path> + Debug,
        file_uploader_sender: tokio::sync::mpsc::Sender<FileUploaderCommand>,
        drive: GoogleDrive,
        cache_dir: PathBuf,
        settings: SyncSettings,
    ) -> Result<DriveFilesystem> {
        let config_path = config_path.as_ref();
        debug!(
            "DriveFilesystem::new(config_path: {})",
            config_path.display()
        );
        // let upload_filter = CommonFileFilter::from_path(config_path)?;
        let mut entries = HashMap::new();
        Self::add_root_entry(&mut entries);

        let changes_start_token = drive.get_start_page_token().await?;

        let mut s = Self {
            source: drive,
            cache_dir: Some(cache_dir),
            entries,
            file_uploader_sender,
            /*TODO: implement a way to increase this if necessary*/
            generation: 0,
            children: HashMap::new(),
            settings,
            changes_start_token,
            last_checked_changes: UNIX_EPOCH,
            ino_drive_id: BiMap::new(),
        };
        s.ino_drive_id.insert(FUSE_ROOT_ID.into(), DriveId::root());
        Ok(s)
    }

    fn add_root_entry(entries: &mut HashMap<DriveId, DriveEntry>) {
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
            DriveId::root(),
            DriveEntry::new(inode, "root".to_string(), DriveId::root(), root_attr, None),
        );
    }
    #[instrument(fields(% self, inode))]
    fn get_cache_dir_for_file(&self, inode: DriveId) -> Result<PathBuf> {
        debug!("get_cache_dir_for_file: {}", inode);
        let cache_dir = self.cache_dir.as_ref().ok_or(anyhow!("no cache dir"))?;
        debug!(
            "get_cache_dir_for_file: {}, cache_dir: {}",
            inode,
            cache_dir.display()
        );
        let entry = self
            .entries
            .get(&inode)
            .ok_or(anyhow!("could not get entry"))?;
        debug!(
            "get_cache_dir_for_file: entry local_path: {:?}",
            entry.local_path
        );
        let path = Self::construct_cache_folder_path(cache_dir, entry);
        debug!("get_cache_dir_for_file: {}: {}", inode, path.display());
        Ok(path)
    }

    #[instrument]
    fn construct_cache_folder_path(cache_dir: &Path, entry: &DriveEntry) -> PathBuf {
        let path = cache_dir.to_path_buf();
        path.join(match entry.local_path.as_ref() {
            Some(x) => match x.parent() {
                Some(parent) => parent.to_path_buf(),
                None => PathBuf::new(),
            },
            None => PathBuf::new(),
        })
    }
    #[instrument(fields(% self))]
    async fn add_all_file_entries(&mut self) -> anyhow::Result<()> {
        let old_len = self.entries.len();
        self.entries = HashMap::new();
        let mut entries = HashMap::new();
        self.children = HashMap::new();
        self.ino_drive_id = BiMap::new();
        self.ino_drive_id
            .insert(Inode::from(FUSE_ROOT_ID), DriveId::root());
        let alternative_rood_id = self
            .source
            .get_metadata_for_file(DriveId::root())
            .await?
            .id
            .context("the root id is not available")?;

        Self::add_root_entry(&mut entries);
        let drive_entries = self.source.list_all_files().await?;
        for metadata in drive_entries {
            let inode = self.generate_ino_with_offset(entries.len());
            let entry = self.create_entry_from_drive_metadata(&metadata, inode);
            if let Ok(entry) = entry {
                let inode = entry.ino.clone();
                debug!(
                    "add_all_file_entries: adding entry: ({}) {:?}",
                    inode, entry
                );
                let drive_id = entry.drive_id.clone();
                entries.insert(drive_id.clone(), entry);
                self.ino_drive_id.insert(inode, drive_id.clone());
                if let Some(parents) = metadata.parents {
                    debug!(
                        "drive_id: {:<40} has parents: {}",
                        drive_id.to_string(),
                        parents.len()
                    );
                    let parents = parents.iter().map(|p| DriveId::from(p));
                    for parent in parents {
                        if parent.to_string() == alternative_rood_id.to_string() {
                            debug!("drive_id: {:<40} has parent: {:<40} which is the alternative root id, skipping", drive_id.to_string(), parent.to_string());
                            self.add_child(drive_id.clone(), &DriveId::root());
                            continue;
                        }
                        self.add_child(drive_id.clone(), &parent);
                    }
                } else {
                    debug!(
                        "drive_id: {:<40} does not have parents",
                        drive_id.to_string()
                    );
                    //does not belong to any folder, add to root
                    self.add_child(drive_id, &DriveId::root());
                }
            } else {
                warn!(
                    "add_all_file_entries: could not create entry! err: {:?} metadata:{:?}",
                    entry, metadata
                );
            }
        }
        debug!(
            "add_all_file_entries: entries: new len: {} old len: {}",
            entries.len(),
            old_len
        );
        self.entries = entries;
        debug!("build all local paths");
        self.get_entry_mut(&DriveId::root())
            .expect("The root entry has to exist by now")
            .build_local_path(None);
        self.build_path_for_children(&DriveId::root());
        Ok(())
    }
    #[instrument(skip(self))]
    fn build_path_for_children(&mut self, parent_id: &DriveId) {
        let parent = self
            .entries
            .get(parent_id)
            .expect("parent entry has to exist");
        debug!(
            "build_path_for_children: parent: {:<40} => {:?}",
            parent.drive_id.to_string(),
            parent.name
        );
        if let Some(child_list) = self.children.get(&parent_id) {
            debug!(
                "build_path_for_children: ({}) child_list: {:?}",
                child_list.len(),
                child_list
            );
            for child_id in child_list.clone() {
                let parent: Option<LocalPath> = match self.entries.get(parent_id) {
                    Some(e) => e.local_path.clone(),
                    None => None,
                };
                let child = self.entries.get_mut(&child_id);
                if let Some(child) = child {
                    child.build_local_path(parent);
                } else {
                    warn!("add_all_file_entries: could not find child entry!");
                }
                debug!(
                    "build_path_for_children: child: {:?} parent: {:?}",
                    child_id, parent_id
                );
                self.build_path_for_children(&child_id);
            }
        }
    }

    #[instrument(skip(self), fields(self.children.len = % self.children.len()))]
    fn add_child(&mut self, drive_id: DriveId, parent: &DriveId) {
        let existing_child_list = self.children.get_mut(&parent);
        if let Some(existing_child_list) = existing_child_list {
            debug!(
                "add_child: adding child: {:?} to parent: {:?}",
                drive_id, parent
            );
            existing_child_list.push(drive_id);
        } else {
            debug!(
                "add_child: adding child: {:?} to parent: {:?} (new)",
                drive_id, parent
            );
            let set = vec![drive_id];
            self.children.insert(parent.clone(), set);
        }
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
    fn get_drive_id(&self, ino: impl Into<Inode>) -> Option<&DriveId> {
        self.ino_drive_id.get_by_left(&ino.into())
    }

    #[instrument(fields(% self, inode))]
    fn create_entry_from_drive_metadata(
        &mut self,
        metadata: &File,
        inode: Inode,
    ) -> anyhow::Result<DriveEntry> {
        let name = metadata.name.as_ref().ok_or_else(|| "no name");
        if let Err(e) = name {
            warn!("could not get name: {}", e);
            return Err(anyhow!("could not get name: {}", e));
        }
        let name = name.as_ref().unwrap();
        if name.contains("/") || name.contains("\\") || name.contains(":") || name.contains("'") {
            warn!("invalid name: {}", name);
            return Err(anyhow!("invalid name"));
        }
        let ino = inode;
        let id = DriveId::from(metadata.id.as_ref().context("could not get id")?);
        let mime_type = metadata.mime_type.as_ref().context(
            "could not determine if this is a file or a folder since the mime type was empty",
        )?;
        let kind = match mime_type.as_str() {
            "application/vnd.google-apps.document"
            | "application/vnd.google-apps.spreadsheet"
            | "application/vnd.google-apps.drawing"
            | "application/vnd.google-apps.form"
            | "application/vnd.google-apps.presentation"
            | "application/vnd.google-apps.drive-sdk"
            | "application/vnd.google-apps.script"
            //TODO: add all relevant mime types or match only the start or something
            => return Err(anyhow!("google app files are not supported (docs, sheets, etc)")),
            "application/vnd.google-apps.folder" => FileType::Directory,
            _ => FileType::RegularFile,
        };
        let permissions = self.get_file_permissions(&id, &kind);
        debug!("created time: {:?}", metadata.created_time);
        debug!("modified time: {:?}", metadata.modified_time);
        debug!("viewed by me time: {:?}", metadata.viewed_by_me_time);
        let attributes = FileAttr {
            ino: ino.into(),
            size: Self::get_size_from_drive_metadata(metadata).unwrap_or(0),
            blocks: 0,
            atime: metadata
                .viewed_by_me_time
                .map(SystemTime::from)
                .unwrap_or(UNIX_EPOCH),
            mtime: metadata
                .modified_time
                .map(SystemTime::from)
                .unwrap_or(UNIX_EPOCH),
            ctime: SystemTime::now(),
            crtime: metadata
                .created_time
                .map(SystemTime::from)
                .unwrap_or(UNIX_EPOCH),
            kind,
            perm: permissions,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        };

        let entry = DriveEntry::new(ino, name, id, attributes, Some(metadata.clone()));

        Ok(entry)
    }
    #[instrument(fields(% self))]
    fn get_file_permissions(&self, _drive_id: &DriveId, file_kind: &FileType) -> u16 {
        //TODO: actually get the permissions from a default or some config for each file etc, not just these hardcoded ones
        if file_kind == &FileType::Directory {
            return 0o755;
        }
        return 0o644;
    }
}
// endregion

// region caching
impl DriveFilesystem {
    async fn download_file_to_cache(&mut self, ino: impl Into<DriveId>) -> Result<PathBuf> {
        let ino = ino.into();
        debug!("download_file_to_cache: {}", ino);
        let entry = self.get_entry_r(&ino)?;
        let drive_id = entry.drive_id.clone();
        let drive = &self.source;
        let cache_path = self.get_cache_path_for_entry(&entry)?;
        let folder = cache_path.parent().ok_or(anyhow!(
            "could not get the folder the cache file should be saved in"
        ))?;
        if !folder.exists() {
            debug!("creating folder: {}", folder.display());
            std::fs::create_dir_all(folder)?;
        }
        debug!("downloading file: {}", cache_path.display());
        let metadata = drive.download_file(drive_id, &cache_path).await?;
        debug!("downloaded file: {}", cache_path.display());
        self.set_entry_metadata_with_ino(&ino, metadata)?;
        // self.set_entry_content_up_to_date(&ino)?;
        Ok(cache_path)
    }

    #[instrument(fields(% self))]
    async fn update_entry_metadata_cache_if_needed(&mut self) -> Result<Vec<DriveId>> {
        debug!("getting changes...");
        let changes = self.get_changes().await?;
        debug!("got changes: {}", changes.len());
        let mut updated_entries = Vec::new();
        for change in changes {
            debug!("processing change: {:?}", change);
            match change.kind {
                ChangeType::Drive(drive) => {
                    warn!("im not sure how to handle drive changes: {:?}", drive);

                    updated_entries.push(change.id);
                    continue;
                }
                ChangeType::File(file) => {
                    debug!("file change: {:?}", file);
                    let drive_id = &change.id;

                    let entry = self.entries.get_mut(drive_id);
                    if let Some(entry) = entry {
                        debug!(
                            "updating entry metadata: {}, {:?} entry: {:?}",
                            entry.ino, entry.md5_checksum, entry
                        );
                        let change_successful = Self::update_entry_metadata(file, entry);
                        if let Err(e) = change_successful {
                            warn!("got an err while update entry metadata: {}", e);
                            updated_entries.push(change.id);
                            continue;
                        }
                    }

                    updated_entries.push(change.id);
                    debug!("processed change");
                    continue;
                }
                ChangeType::Removed => {
                    debug!("removing entry: {:?}", change);
                    //TODO: actually delete the entry
                    self.remove_entry(&change.id)?;
                    updated_entries.push(change.id);
                    continue;
                }
            }
        }
        debug!("updated entry metadata cache");
        Ok(updated_entries)
    }

    /// Updates the entry from the drive if needed
    ///
    /// returns true if the entry's metadata was updated from the drive
    #[instrument(fields(% self))]
    async fn update_cache_if_needed(&mut self, ino: impl Into<Inode> + Debug) -> Result<bool> {
        let ino = ino.into();
        let drive_id = self.get_drive_id_from_ino(ino)?.clone();
        let metadata_updated = self.update_entry_metadata_cache_if_needed().await?;
        let metadata_updated = metadata_updated.contains(&drive_id);
        let entry = match self.get_entry_r(&drive_id) {
            Ok(entry) => entry,
            Err(e) => {
                warn!("could not get entry: {}", e);
                return Err(e);
            }
        };
        if entry.has_upstream_content_changes {
            debug!("entry has upstream changes: {}, downloading...", ino);
            self.download_file_to_cache(drive_id).await?;
            return Ok(metadata_updated);
        }
        Ok(metadata_updated)
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
        debug!(
            "construct_cache_path_for_entry: {} with cache_dir: {}",
            entry.ino,
            cache_dir.display()
        );
        let path = Self::construct_cache_folder_path(cache_dir, entry).join(&entry.name);
        debug!(
            "get_cache_path_for_entry: {}: {}",
            entry.ino,
            path.display()
        );
        path
    }

    fn set_entry_metadata_with_ino(
        &mut self,
        ino: impl Into<DriveId>,
        drive_metadata: File,
    ) -> anyhow::Result<()> {
        let entry = self.get_entry_mut(ino).context("no entry with ino")?;

        Self::update_entry_metadata(drive_metadata, entry)
    }

    #[instrument]
    fn update_entry_metadata(drive_metadata: File, entry: &mut DriveEntry) -> anyhow::Result<()> {
        if let Some(name) = drive_metadata.name {
            entry.name = OsString::from(name);
        }
        if let Some(size) = drive_metadata.size {
            entry.attr.size = size as u64;
        }
        if let Some(modified_time) = drive_metadata.modified_time {
            entry.attr.mtime = modified_time.into();
        }
        if let Some(created_time) = drive_metadata.created_time {
            entry.attr.ctime = created_time.into();
        }
        if let Some(viewed_by_me) = drive_metadata.viewed_by_me_time {
            entry.attr.atime = viewed_by_me.into();
        }

        let checksum_mismatch = Self::compare_checksums(&drive_metadata.md5_checksum, &entry);
        match checksum_mismatch {
            ChecksumMatch::Missing | ChecksumMatch::Unknown | ChecksumMatch::RemoteMismatch => {
                debug!(
                    "md5_checksum mismatch: {:?} != {:?}",
                    drive_metadata.md5_checksum, entry.md5_checksum
                );
                entry.set_md5_checksum(drive_metadata.md5_checksum);
                entry.has_upstream_content_changes = true;
                debug!(
                    "updated md5_checksum of {} to: {:?}",
                    entry.ino, &entry.md5_checksum
                );
            }

            ChecksumMatch::Match => {
                debug!(
                    "md5_checksum match: {:?} == {:?}",
                    drive_metadata.md5_checksum, &entry.md5_checksum
                );
                entry.has_upstream_content_changes = false;
            }

            ChecksumMatch::CacheMismatch => {
                debug!(
                    "the local checksum and the remote checksum match,\
                 so we can assume the local changes have just been uploaded to the remote"
                );
                entry.has_upstream_content_changes = false;
            }

            ChecksumMatch::LocalMismatch => {
                debug!(
                    "the local checksum does not match the remote or the cached \
                checksum, this means the local file has been modified"
                );
                entry.has_upstream_content_changes = false;
            }

            ChecksumMatch::Conflict => {
                error!("ChecksumMatch::Conflict! the local file has been modified and the remote file has been modified");
                Self::print_message_to_user(
                    "ChecksumMatch::Conflict! the local file has been modified and the remote file has been modified",
                );
                let input: String = Self::get_input_from_user("press 1 to overwrite the local file with the remote file, press 2 to overwrite the remote file with the local file", vec!["1", "2"]);
                //TODO: conflict resolving is not working correctly!
                // it asks the user for input, then downloads the file but proceeds to write to the local file
                // and then asks the user for input again. in the end when both times the user chose to overwrite
                // the local file with the remote file, the local and remote are a mix of both files, which is not
                // what we want.
                if input == "1" {
                    debug!("overwriting the local file with the remote file");
                    entry.has_upstream_content_changes = true;
                } else {
                    debug!("overwriting the remote file with the local file");
                    entry.has_upstream_content_changes = false;
                }
            }
        };
        Ok(())
    }

    /// Compares the md5_checksum of the entry (local & cache) with the given md5_checksum.
    #[instrument(skip(entry), fields(entry.ino = % entry.ino, entry.md5_checksum = entry.md5_checksum))]
    fn compare_checksums(md5_checksum: &Option<String>, entry: &DriveEntry) -> ChecksumMatch {
        if md5_checksum.is_none() {
            warn!("no remote md5_checksum, can't compare, treating as a missing");
            return ChecksumMatch::Missing;
        }
        if md5_checksum == &entry.local_md5_checksum && md5_checksum == &entry.md5_checksum {
            debug!("md5_checksum match: (r) == (l) == (c): {:?} ", md5_checksum);
            return ChecksumMatch::Match;
        }
        if md5_checksum != &entry.local_md5_checksum
            && md5_checksum != &entry.md5_checksum
            && entry.local_md5_checksum != entry.md5_checksum
        {
            debug!(
                "md5_checksum match: {:?} (r) != {:?} (l) != {:?} (c)",
                md5_checksum, entry.local_md5_checksum, entry.md5_checksum
            );
            return ChecksumMatch::Conflict;
        }

        if md5_checksum == &entry.md5_checksum {
            debug!("md5_checksum match: (r) == (c): {:?}", md5_checksum);
            return ChecksumMatch::LocalMismatch;
        }
        if md5_checksum == &entry.local_md5_checksum {
            debug!("md5_checksum match: (r) == (l): {:?}", md5_checksum);
            return ChecksumMatch::CacheMismatch;
        }
        if &entry.local_md5_checksum == &entry.md5_checksum {
            debug!(
                "md5_checksum match: (l) == (c): {:?} ",
                entry.local_md5_checksum
            );
            return ChecksumMatch::RemoteMismatch;
        }
        warn!("how could I get here?");
        debug(md5_checksum);
        debug(entry);
        //TODO: make sure this case does not happen
        return ChecksumMatch::Unknown;
    }

    #[instrument]
    fn compute_md5_checksum(path: &PathBuf) -> Option<String> {
        use md5::{Digest, Md5};
        use std::{fs, io};
        debug!("computing md5_checksum for {}", path.display());
        let mut file = fs::File::open(&path).ok()?;
        let mut hasher = Md5::new();
        let _n = io::copy(&mut file, &mut hasher).ok()?;
        let hash = hasher.finalize();
        let hash = format!("{:x}", hash);
        debug!("computed md5_checksum for {}: {}", path.display(), hash);
        Some(hash)
    }
    async fn get_changes(&mut self) -> anyhow::Result<Vec<Change>> {
        if self.last_checked_changes + self.settings.cache_time() > SystemTime::now() {
            debug!("not checking for changes since we already checked recently");
            return Ok(vec![]);
        }
        debug!("checking for changes...");
        let changes: anyhow::Result<Vec<Change>> = self
            .source
            .get_changes_since(&mut self.changes_start_token)
            .await?
            .into_iter()
            .map(Change::try_from)
            .collect();

        self.last_checked_changes = SystemTime::now();
        debug!(
            "checked for changes, found {} changes",
            changes.as_ref().unwrap_or(&Vec::<Change>::new()).len()
        );
        changes
    }
    fn remove_entry(&mut self, id: &DriveId) -> anyhow::Result<()> {
        let _entry = self.entries.remove_entry(&id);

        //TODO: remove from children
        //TODO: remove from cache if it exists
        Ok(())
    }
    fn get_input_from_user(message: &str, options: Vec<&str>) -> String {
        let mut input = String::new();
        loop {
            Self::print_message_to_user(message);
            let size_read = std::io::stdin().read_line(&mut input);
            if let Ok(size_read) = size_read {
                if size_read > 0 {
                    let input = input.trim();
                    if options.contains(&input) {
                        return input.to_string();
                    }
                }
                Self::print_message_to_user("invalid input, please try again");
            } else {
                error!("could not read input from user: {:?}", size_read);
            }
        }
    }
    fn print_message_to_user(message: &str) {
        let _x = stdout().write_all(format!("{}\n", message).as_bytes());
        let _x = stdout().flush();
    }
}

// endregion

// region common
impl DriveFilesystem {
    fn generate_ino_with_offset(&self, offset: usize) -> Inode {
        Inode::new((self.entries.len() + 10 + offset) as u64)
    }

    fn get_entry_mut(&mut self, ino: impl Into<DriveId>) -> Option<&mut DriveEntry> {
        self.entries.get_mut(&ino.into())
    }

    fn get_entry_r<'a>(&self, ino: impl Into<&'a DriveId>) -> Result<&DriveEntry> {
        let ino = ino.into();
        self.entries
            .get(ino)
            .ok_or(anyhow!("Entry not found").into())
    }
}

// endregion

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

        // let root = self.root.to_path_buf();
        // let x = run_async_blocking(self.add_dir_entry(&root, Inode::from(FUSE_ROOT_ID), true));
        let x = run_async_blocking(self.add_all_file_entries());
        if let Err(e) = x {
            error!("could not add entries: {}", e);
        }
        for (id, entry) in self.entries.iter() {
            debug!("entry: {:<40} => {:?}", id.to_string(), entry);
        }

        debug!("init done");
        Ok(())
    }
    //endregion
    //region destroy
    #[instrument(fields(% self))]
    fn destroy(&mut self) {
        debug!("destroy");
        let stop_res =
            run_async_blocking(self.file_uploader_sender.send(FileUploaderCommand::Stop));
        if let Err(e) = stop_res {
            error!("could not send stop command to file uploader: {}", e);
        }
    }
    //endregion
    //region lookup
    #[instrument(skip(_req, reply), fields(% self))]
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup: {}:{:?}", parent, name);
        let update_res = run_async_blocking(self.update_entry_metadata_cache_if_needed());
        if let Err(e) = update_res {
            error!("read: could not update metadata cache: {}", e);
            reply.error(libc::EIO);
            return;
        }
        let parent = parent.into();
        let parent_drive_id = self.get_drive_id_from_ino(&parent);
        if parent_drive_id.is_err() {
            warn!(
                "lookup: could not get drive_id for {}: {:?}",
                parent, parent_drive_id
            );
            reply.error(libc::ENOENT);
            return;
        }
        let parent_drive_id = parent_drive_id.unwrap();
        let children = self.children.get(&parent_drive_id);
        if children.is_none() {
            warn!(
                "lookup: could not find children for {}: {}",
                parent, parent_drive_id
            );
            for (id, entry) in self.entries.iter() {
                debug!("entry: {:<40} => {:?}", id.to_string(), entry);
            }
            reply.error(libc::ENOENT);
            return;
        }
        let children = children.unwrap();
        debug!("lookup: children: {:?}", children);
        for child_inode in children {
            let entry = self.entries.get(&child_inode);
            if entry.is_none() {
                warn!("lookup: could not find entry for {}", child_inode);
                continue;
            }
            let entry = entry.unwrap();

            let path: PathBuf = entry.name.clone().into();
            let accepted = name.eq_ignore_ascii_case(&path);
            debug!(
                "entry: {}:(accepted={}),{:?}; {:?}; {:?}",
                child_inode, accepted, entry.md5_checksum, path, entry.attr
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
        let update_res = run_async_blocking(self.update_entry_metadata_cache_if_needed());
        if let Err(e) = update_res {
            error!("read: could not update metadata cache: {}", e);
            reply.error(libc::EIO);
            return;
        }
        debug!("getattr: after update_entry_metadata_cache_if_needed");
        let drive_id = self.get_drive_id_from_ino(&ino.into());
        if drive_id.is_err() {
            warn!("readdir: could not get drive id for ino: {}", ino);
            reply.error(libc::ENOENT);
            return;
        }
        let drive_id = drive_id.unwrap();
        let entry = self.entries.get(drive_id);
        if let Some(entry) = entry {
            reply.attr(&self.settings.time_to_live(), &entry.attr);
        } else {
            reply.error(libc::ENOENT);
        }
        debug!("getattr: done")
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
        let drive_id = self.get_drive_id_from_ino(ino);
        if drive_id.is_err() {
            warn!("readdir: could not get drive id for ino: {}", ino);
            reply.error(libc::ENOENT);
            return;
        }
        let drive_id = drive_id.unwrap().clone();
        let entry = self.get_entry_mut(drive_id);
        if let None = entry {
            error!("setattr: could not find entry for {}", ino);
            reply.error(libc::ENOENT);
            return;
        }
        let entry = entry.unwrap();
        let attr = &mut entry.attr;

        if let Some(mode) = mode {
            debug!("setting perm from {} to {}", attr.perm, mode);
            attr.perm = mode as u16;
        }
        if let Some(uid) = uid {
            debug!("setting uid from {} to {}", attr.uid, uid);
            attr.uid = uid;
        }
        if let Some(gid) = gid {
            debug!("setting gid from {} to {}", attr.gid, gid);
            attr.gid = gid;
        }
        if let Some(size) = size {
            debug!("setting size from {} to {}", attr.size, size);
            attr.size = size;
        }
        if let Some(flags) = flags {
            debug!("setting flags from {} to {}", attr.flags, flags);
            attr.flags = flags;
        }
        reply.attr(&ttl, &attr);
        //TODO: update file on drive if necessary
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

        let update_res = run_async_blocking(self.update_entry_metadata_cache_if_needed());
        if let Err(e) = update_res {
            error!("read: could not update metadata cache: {}", e);
            reply.error(libc::EIO);
            return;
        }
        let x: Result<bool> = run_async_blocking(self.update_cache_if_needed(ino));
        if let Err(e) = x {
            error!("read: could not update cache: {}", e);
            reply.error(libc::EIO);
            return;
        }

        let drive_id = self.get_drive_id_from_ino(&ino.into());
        if drive_id.is_err() {
            warn!("readdir: could not get drive id for ino: {}", ino);
            reply.error(libc::ENOENT);
            return;
        }
        let drive_id = drive_id.unwrap();
        let entry = self.get_entry_r(drive_id);
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
        let file = file.unwrap();

        let mut buf = vec![0; size as usize];
        debug!("reading file: {:?} at {} with size {}", &path, offset, size);
        file.read_exact_at(&mut buf, offset as u64).unwrap();
        debug!("read file: {:?} at {}", &path, offset);
        reply.data(&buf);
    }
    //endregion
    //region write
    #[instrument(skip(_req, reply), fields(% self, data = data.len()))]
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
        let cache_update_success: Result<bool> =
            run_async_blocking(self.update_cache_if_needed(ino));
        match cache_update_success {
            Err(e) => {
                error!("write: could not update cache: {}", e);
                reply.error(libc::EIO);
                return;
            }
            Ok(cache_updated) => {
                if cache_updated {
                    error!("conflict detected, upstream had a change, cache was updated, aborting write");
                    //TODO: maybe output a message to the user?
                    reply.error(libc::EIO);
                    return;
                }
            }
        }
        let cache_dir = self.cache_dir.as_ref().map(|s| s.to_path_buf());
        if let None = cache_dir {
            error!("write: cache dir not set");
            reply.error(libc::ENOENT);
            return;
        }
        let cache_dir = cache_dir.unwrap();
        {
            let drive_id = self.get_drive_id_from_ino(&ino.into());
            if drive_id.is_err() {
                warn!("readdir: could not get drive id for ino: {}", ino);
                reply.error(libc::ENOENT);
                return;
            }
            let drive_id = drive_id.unwrap().clone();
            let entry = self.get_entry_mut(drive_id);
            if let None = entry {
                error!("write: could not find entry for {}", ino);
                reply.error(libc::ENOENT);
                return;
            }
            let mut entry = entry.unwrap();
            //TODO: queue uploads on a separate thread

            let path = Self::construct_cache_path_for_entry(&cache_dir, &entry);
            // let path = entry.local_path.to_path_buf();
            let truncate = flags & libc::O_TRUNC != 0 || entry.attr.size == 0;
            debug!("truncate: {} because: (flags({}) & libc::O_TRUNC != 0) = {} or (entry.attr.size({}) == 0) = {}", truncate, flags, flags & libc::O_TRUNC != 0, entry.attr.size, entry.attr.size == 0);
            debug!("opening file: truncate({}) {:?}", truncate, &path);
            let file = OpenOptions::new()
                .truncate(truncate)
                .create(true)
                .write(true)
                .open(&path);
            if let Err(e) = file {
                error!("write: could not open file: {:?}: {}", path, e);
                reply.error(libc::ENOENT);
                return;
            }
            let mut file = file.unwrap();

            debug!(
                "writing file: {:?} at {} with  size {}",
                &path,
                offset,
                data.len()
            );

            file.seek(SeekFrom::Start(offset as u64)).unwrap();
            file.write_all(data).unwrap();
            let size = data.len();
            debug!(
                "wrote   file: {:?} at {}; wrote {} bytes",
                &path, offset, size
            );
            reply.written(size as u32);
            debug!(
                "updating size to {} for entry: {:?}",
                entry.attr.size, entry
            );
            let mut attr = &mut entry.attr;
            if truncate {
                attr.size = size as u64;
            } else {
                attr.size = attr.size.max(offset as u64 + size as u64);
            }
            let now = SystemTime::now();
            attr.mtime = now;
            attr.ctime = now;
            debug!(
                "updated  size to {} for entry: {:?}",
                entry.attr.size, entry
            );
            entry.local_md5_checksum = Self::compute_md5_checksum(&path);
            debug!(
                "updated local md5 to {:?} for entry: {:?}",
                entry.local_md5_checksum, entry
            );
            debug!("write done for entry: {:?}", entry);
        }

        let drive_id = self.get_drive_id_from_ino(&ino.into());
        if drive_id.is_err() {
            warn!("readdir: could not get drive id for ino: {}", ino);
            return;
        }
        let drive_id = drive_id.unwrap();
        let entry = self
            .get_entry_r(drive_id)
            .expect("how could this happen to me. I swear it was there a second ago");
        let schedule_res = run_async_blocking(self.schedule_upload(&entry));
        if let Err(e) = schedule_res {
            error!("read: could not schedule the upload: {}", e);
            return;
        }
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
        let update_res = run_async_blocking(self.update_entry_metadata_cache_if_needed());
        if let Err(e) = update_res {
            error!("read: could not update metadata cache: {}", e);
            reply.error(libc::EIO);
            return;
        }
        let drive_id = self.get_drive_id_from_ino(&ino.into());
        if drive_id.is_err() {
            warn!("readdir: could not get drive id for ino: {}", ino);
            reply.error(libc::ENOENT);
            return;
        }
        let drive_id = drive_id.unwrap();
        if let Some(attr) = self.entries.get(drive_id).map(|entry| entry.attr) {
            if attr.kind != FileType::Directory {
                reply.error(libc::ENOTDIR);
                return;
            }
        }
        let dir_drive_id = self.get_drive_id_from_ino(&ino.into());
        if dir_drive_id.is_err() {
            warn!("readdir: could not get drive id for ino: {}", ino);
            reply.error(libc::ENOENT);
            return;
        }
        let dir_drive_id = dir_drive_id.unwrap();
        let children = self.children.get(&dir_drive_id);
        if children.is_none() {
            reply.error(libc::ENOENT);
            return;
        }
        let children = children.unwrap();
        debug(children);
        debug!("children ({}): {:?}", children.len(), children);
        for child_id in children.iter().skip(offset as usize) {
            let entry = self.entries.get(child_id);
            if let Some(entry) = entry {
                if let Some(local_path) = entry.local_path.as_ref() {
                    let path: PathBuf = local_path.clone().into();
                    let attr = entry.attr;
                    let inode = self.get_ino_from_drive_id(child_id);
                    if let Ok(inode) = inode {
                        // Increment the offset for each processed entry
                        offset += 1;
                        debug!("entry: {}:{:?}; {:?}", inode, path, attr);
                        if reply.add((*inode).into(), offset, attr.kind, &entry.name) {
                            // If the buffer is full, we need to stop
                            debug!("readdir: buffer full");
                            break;
                        }
                    }
                }
            }
        }
        debug!("readdir: ok");
        reply.ok();
    }
    //endregion
    //region access
    #[instrument(fields(% self, ino, mask))]
    fn access(&mut self, _req: &Request<'_>, _ino: u64, _mask: i32, reply: ReplyEmpty) {
        reply.ok(); //TODO: implement this correctly
    }
    //endregion
}
//endregion

//TODOs:
// TODO: implement rename/move
// TODO: implement create
// TODO: implement delete
