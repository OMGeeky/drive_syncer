use std::fmt::Display;
use std::ops::Deref;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Inode(u64);

impl Inode {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    pub fn get(&self) -> u64 {
        self.0
    }
}

impl Display for Inode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Into<u64> for Inode {
    fn into(self) -> u64 {
        self.0
    }
}

impl TryInto<u32> for Inode {
    type Error = std::num::TryFromIntError;

    fn try_into(self) -> Result<u32, Self::Error> {
        self.0.try_into()
    }
}

impl From<u64> for Inode {
    fn from(value: u64) -> Inode {
        Inode(value)
    }
}

impl From<u32> for Inode {
    fn from(value: u32) -> Inode {
        Inode(value as u64)
    }
}

impl From<&Inode> for Inode {
    fn from(value: &Inode) -> Self {
        value.clone()
    }
}

impl Deref for Inode {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
