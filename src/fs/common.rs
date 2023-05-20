use crate::async_helper::run_async_blocking;
use crate::common::LocalPath;
use crate::fs::inode::Inode;
use crate::google_drive::DriveId;
use crate::prelude::*;
use anyhow::anyhow;
use async_trait::async_trait;
use fuser::{FileAttr, FileType, TimeOrNow, FUSE_ROOT_ID};
use tracing::debug;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub trait CommonEntry {
    fn get_ino(&self) -> Inode;
    fn get_name(&self) -> &OsStr;
    fn get_local_path(&self) -> &LocalPath;
    fn get_attr(&self) -> &FileAttr;

    // fn new(
    //     ino: impl Into<Inode>,
    //     name: impl Into<OsString>,
    //     local_path: impl Into<LocalPath>,
    //     attr: FileAttr,
    // ) -> Self;
}
#[async_trait]
pub trait CommonFilesystem<Entry: CommonEntry > {
    fn get_entries(&self) -> &HashMap<Inode, Entry>;
    fn get_entries_mut(&mut self) -> &mut HashMap<Inode, Entry>;
    fn get_children(&self) -> &HashMap<Inode, Vec<Inode>>;
    fn get_children_mut(&mut self) -> &mut HashMap<Inode, Vec<Inode>>;
    fn get_root_path(&self) -> LocalPath;

    fn generate_ino(&self) -> Inode {
        Inode::new(self.get_entries().len() as u64 + 1) //TODO: check if this is working or if concurrency is a problem
    }

    fn get_path_from_ino(&self, ino: impl Into<Inode>) -> Option<LocalPath> {
        let ino = ino.into();
        debug!("get_path_from_ino: {}", ino);
        let res = self.get_entry(ino)?.get_local_path().clone();
        debug!("get_path_from_ino: {}:{:?}", ino, res);
        Some(res)
    }

    fn get_full_path_from_ino(&self, ino: impl Into<Inode>) -> Option<LocalPath> {
        let ino = ino.into();
        debug!("get_full_path_from_ino: {}", ino);
        if ino == FUSE_ROOT_ID.into() {
            return Some(self.get_root_path());
        }
        let parent = self.get_parent_ino(ino);
        if let Some(parent) = parent {
            let path: PathBuf = self.get_full_path_from_ino(parent)?.into();
            let buf: LocalPath = path
                .join::<PathBuf>(self.get_path_from_ino(ino)?.into())
                .into();
            debug!("get_full_path_from_ino: {}:{:?}", ino, buf);
            return Some(buf);
        }
        match self.get_path_from_ino(ino) {
            Some(path) => Some(path.clone()),
            None => None,
        }
    }

    fn get_child_with_path(
        &self,
        parent: impl Into<Inode>,
        path: impl AsRef<OsStr>,
    ) -> Option<Inode> {
        let parent = parent.into();
        let path = path.as_ref();
        debug!("get_child_with_path: {}:{:?}", parent, path);
        let children = self.get_children().get(&parent)?;
        let mut res = None;
        for child in children {
            let child_path: &OsStr = self.get_entry(*child)?.get_local_path().into();
            if child_path == path {
                res = Some(*child);
                break;
            }
        }
        debug!("get_child_with_path: {}:{:?}", parent, res);
        res
    }

    fn get_parent_ino(&self, ino: impl Into<Inode>) -> Option<Inode> {
        let ino = ino.into();
        debug!("get_parent_ino: {}", ino);
        if ino == FUSE_ROOT_ID.into() {
            return None;
        }
        let mut parent = None;
        for (parent_ino, child_inos) in self.get_children().iter() {
            if child_inos.contains(&ino) {
                parent = Some(*parent_ino);
                break;
            }
        }
        parent
    }

    fn convert_to_system_time(mtime: TimeOrNow) -> SystemTime {
        let mtime = match mtime {
            TimeOrNow::SpecificTime(t) => t,
            TimeOrNow::Now => SystemTime::now(),
        };
        mtime
    }

    fn get_entry(&self, ino: impl Into<Inode>) -> Option<&Entry> {
        self.get_entries().get(&ino.into())
    }
    fn get_entry_mut(&mut self, ino: impl Into<Inode>) -> Option<&mut Entry> {
        self.get_entries_mut().get_mut(&ino.into())
    }
    fn get_entry_r(&self, ino: impl Into<Inode>) -> Result<&Entry> {
        self.get_entries()
            .get(&ino.into())
            .ok_or(anyhow!("Entry not found").into())
    }

    async fn add_file_entry(
        &mut self,
        parent: impl Into<Inode> + Send,
        name: &OsStr,
        mode: u16,
        size: u64,
    ) -> Result<Inode> {
        let parent = parent.into();
        debug!("add_file_entry: {}:{:?}; {}", parent, name, mode);

        let ino = self
            .add_entry_new(name, mode, FileType::RegularFile, parent, size)
            .await?;

        Ok(ino)
    }

    async fn add_entry_new(
        &mut self,
        name: &OsStr,
        mode: u16,
        file_type: FileType,
        parent_ino: impl Into<Inode> + Send+ Debug,
        size: u64,
    ) -> Result<Inode>;

    fn add_entry(
        &mut self,
        entry: Entry,
        parent_ino: impl Into<Inode> + Debug,
    ) -> Inode
    where Entry: Debug{
        let ino = entry.get_ino();
        self.get_entries_mut().insert(
            ino,entry,
        );

        self.add_child(parent_ino, &ino);
        ino
    }

    fn add_child(&mut self, parent_ino: impl Into<Inode>, ino: impl Into<Inode>) {
        let parents_child_list = self
            .get_children_mut()
            .entry(parent_ino.into())
            .or_default();
        let ino: Inode = ino.into();
        if !parents_child_list.contains(&ino) {
            parents_child_list.push(ino);
        }
    }
}
