use std::ffi::OsString;
use std::fmt::{Display, Pointer};
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DriveId(OsString);

impl DriveId {
    pub(crate) fn root() -> DriveId {
        DriveId(OsString::from("root"))
    }
    pub fn as_str(&self) -> Option<&str> {
        self.0.to_str()
    }
    pub fn into_string(self) -> Result<String, OsString> {
        self.0.into_string()
    }
}

impl Into<OsString> for DriveId {
    fn into(self) -> OsString {
        self.0
    }
}
impl TryInto<String> for DriveId {
    type Error = OsString;

    fn try_into(self) -> Result<String, Self::Error> {
        self.0.into_string()
    }
}
impl From<OsString> for DriveId {
    fn from(value: OsString) -> Self {
        DriveId(value)
    }
}
impl From<String> for DriveId {
    fn from(value: String) -> Self {
        OsString::from(value).into()
    }
}
impl From<&str> for DriveId {
    fn from(s: &str) -> Self {
        DriveId(OsString::from(s))
    }
}
impl DriveId {
    pub fn new(id: impl Into<OsString>) -> Self {
        Self(id.into())
    }
}