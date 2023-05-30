use tracing::debug;

#[derive(Debug, Copy, Clone, Default)]
pub struct HandleFlags {
    // File status flags used for open() and fcntl() are as follows:
    /// append mode.
    o_append: bool,
    /// [SIO](https://pubs.opengroup.org/onlinepubs/009695399/help/codes.html#SIO) Write according to synchronized I/O data integrity completion.
    o_dsync: bool,
    /// Non-blocking mode.
    o_nonblock: bool,
    /// [SIO](https://pubs.opengroup.org/onlinepubs/009695399/help/codes.html#SIO) Synchronized read I/O operations.
    o_rsync: bool,
    /// Write according to synchronized I/O file integrity completion.
    o_sync: bool,

    // Mask for use with file access modes is as follows:
    /// Mask for file access modes.
    // O_ACCMODE

    // File access modes used for open() and fcntl() are as follows:

    /// Open for reading only.
    o_rdonly: bool,
    /// Open for reading and writing.
    o_rdwr: bool,
    /// Open for writing only.
    o_wronly: bool,
}

impl HandleFlags {
    pub(crate) fn can_write(&self) -> bool {
        self.o_wronly || self.o_rdwr
    }

    pub(crate) fn can_read(&self) -> bool {
        self.o_rdonly || self.o_rdwr
    }
}

impl From<i32> for HandleFlags {
    fn from(value: i32) -> Self {
        debug!("Creating HandleFlags from an i32: {:x}", value);
        let s = Self {
            o_append: value & libc::O_APPEND != 0,
            o_dsync: value & libc::O_DSYNC != 0,
            o_nonblock: value & libc::O_NONBLOCK != 0,
            o_rsync: value & libc::O_RSYNC != 0,
            o_sync: value & libc::O_SYNC != 0,
            o_rdonly: value & libc::O_ACCMODE == libc::O_RDONLY,
            o_rdwr: value & libc::O_ACCMODE == libc::O_RDWR,
            o_wronly: value & libc::O_ACCMODE == libc::O_WRONLY,
        };
        #[cfg(test)]
        {
            let o_accmode = value & libc::O_ACCMODE;
            let o_rdonly = o_accmode == libc::O_RDONLY;
            let o_rdwr = o_accmode == libc::O_RDWR;
            let o_wronly = o_accmode == libc::O_WRONLY;
            debug!(
                "accmode {:x} rdonly {} rdwr {} wronly {}",
                o_accmode, o_rdonly, o_rdwr, o_wronly
            );
        }
        debug!("created HandleFlags: {:?}", s);
        s
    }
}

impl Into<i32> for HandleFlags {
    fn into(self) -> i32 {
        let mut flags = 0;
        if self.o_append {
            flags |= libc::O_APPEND;
        }
        if self.o_dsync {
            flags |= libc::O_DSYNC;
        }
        if self.o_nonblock {
            flags |= libc::O_NONBLOCK;
        }
        if self.o_rsync {
            flags |= libc::O_RSYNC;
        }
        if self.o_sync {
            flags |= libc::O_SYNC;
        }
        if self.o_rdonly {
            flags |= libc::O_RDONLY;
        }
        if self.o_rdwr {
            flags |= libc::O_RDWR;
        }
        if self.o_wronly {
            flags |= libc::O_WRONLY;
        }
        flags
    }
}

impl Into<u32> for HandleFlags {
    fn into(self) -> u32 {
        let i_num: i32 = self.into();
        i_num as u32
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handle_flags_ro() {
        crate::tests::init_logs();
        let flags = 0;
        let handle_flags = HandleFlags::from(flags);
        debug!("flags: {:x} => {:?}", flags, handle_flags);
        assert!(handle_flags.can_read());
        assert!(!handle_flags.can_write());
        let flags = 32768;
        let handle_flags = HandleFlags::from(flags);
        debug!("flags: {:x} => {:?}", flags, handle_flags);
        assert!(handle_flags.can_read());
        assert!(!handle_flags.can_write());
    }
    #[test]
    fn handle_flags_wo() {
        crate::tests::init_logs();
        let flags = 1;
        let handle_flags = HandleFlags::from(flags);
        debug!("flags: {:x} => {:?}", flags, handle_flags);
        assert!(handle_flags.can_write());
        assert!(!handle_flags.can_read());
    }
    #[test]
    fn handle_flags_rw() {
        crate::tests::init_logs();
        let flags = 2;
        let handle_flags = HandleFlags::from(flags);
        debug!("flags: {:x} => {:?}", flags, handle_flags);
        debug!("test432");
        assert!(handle_flags.can_write());
        assert!(handle_flags.can_read());
    }
    #[test]
    fn handle_flags_into_rw() {
        crate::tests::init_logs();
        debug!("test123");
        let mut x = HandleFlags::default();
        x.o_rdwr = true;
        assert!(x.can_write());
        assert!(x.can_read());
        let flags: i32 = x.into();
        assert_eq!(2, flags);
    }
}
