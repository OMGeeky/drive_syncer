use std::path::{Path, PathBuf};
use crate::prelude::*;
use ignore::gitignore;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
#[derive(Debug)]
pub struct CommonFileFilter {
    pub filter: Gitignore,
}
impl CommonFileFilter{
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let ignores = GitignoreBuilder::new(&path)
            .build()?;
        let s = Self {
            filter: ignores,
        };
        Ok(s)
    }
    pub fn is_filter_matched(&self, path: &Path) -> Result<bool> {
        Ok(self.filter.matched(path, path.is_dir()).is_ignore())
    }
}
