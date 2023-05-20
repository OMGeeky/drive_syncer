use std::ffi::OsString;
use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DriveId(String);

impl DriveId {
    pub(crate) fn root() -> DriveId {
        DriveId(String::from("root"))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Into<OsString> for DriveId {
    fn into(self) -> OsString {
        OsString::from(self.0)
    }
}

impl TryFrom<OsString> for DriveId {
    type Error = anyhow::Error;
    fn try_from(value: OsString) -> anyhow::Result<Self> {
        let result = value.into_string();
        if let Err(e) = result {
            return Err(anyhow::anyhow!("Failed to convert OsString to String: {:?}", e));
        }
        Ok(DriveId::new(result.unwrap()))
    }
}

impl From<String> for DriveId {
    fn from(value: String) -> Self {
        DriveId::new(value)
    }
}

impl From<&str> for DriveId {
    fn from(s: &str) -> Self {
        DriveId::new(s)
    }
}

impl From<&DriveId> for DriveId {
    fn from(s: &DriveId) -> Self {
        s.clone()
    }
}

impl From<&String> for DriveId {
    fn from(s: &String) -> Self {
        DriveId::new(s)
    }
}

impl DriveId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl Display for DriveId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
