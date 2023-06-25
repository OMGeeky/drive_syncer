use std::ffi::{OsStr, OsString};
use std::fmt::Debug;
use std::ops::Deref;
use std::path::{Path, PathBuf};

//region LocalPath
#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct LocalPath(PathBuf);

impl From<PathBuf> for LocalPath {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}

impl From<&Path> for LocalPath {
    fn from(path: &Path) -> Self {
        Self(path.to_path_buf())
    }
}

impl From<&PathBuf> for LocalPath {
    fn from(path: &PathBuf) -> Self {
        Self(path.to_path_buf())
    }
}

impl From<OsString> for LocalPath {
    fn from(path: OsString) -> Self {
        Self::from(&path)
    }
}

impl From<&OsString> for LocalPath {
    fn from(path: &OsString) -> Self {
        Path::new(path).into()
    }
}

impl<T> AsRef<T> for LocalPath
where
    T: ?Sized,
    <PathBuf as Deref>::Target: AsRef<T>,
{
    fn as_ref(&self) -> &T {
        self.0.deref().as_ref()
    }
}

impl Deref for LocalPath {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
//------------------------------------------

impl From<LocalPath> for PathBuf {
    fn from(value: LocalPath) -> Self {
        value.0
    }
}

impl From<LocalPath> for OsString {
    fn from(value: LocalPath) -> Self {
        value.0.into_os_string()
    }
}

impl<'a> From<&'a LocalPath> for &'a Path {
    fn from(value: &'a LocalPath) -> Self {
        &value.0
    }
}

impl<'a> From<&'a LocalPath> for &'a OsStr {
    fn from(value: &'a LocalPath) -> Self {
        value.0.as_os_str()
    }
}

impl<'a> From<&'a LocalPath> for &'a PathBuf {
    fn from(value: &'a LocalPath) -> Self {
        &value.0
    }
}
//endregion
