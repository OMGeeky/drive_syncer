use std::ffi::{OsStr, OsString};
use std::ops::Deref;
use std::path::{Path, PathBuf};

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

impl Into<PathBuf> for LocalPath {
    fn into(self) -> PathBuf {
        self.0
    }
}
impl Into<OsString> for LocalPath {
    fn into(self) -> OsString {
        self.0.into_os_string()
    }
}
impl<'a> Into<&'a Path> for &'a LocalPath {
    fn into(self) -> &'a Path {
        &self.0
    }
}

impl<'a> Into<&'a OsStr> for &'a LocalPath {
    fn into(self) -> &'a OsStr {
        self.0.as_os_str()
    }
}

impl<'a> Into<&'a PathBuf> for &'a LocalPath {
    fn into(self) -> &'a PathBuf {
        &self.0
    }
}
