use std::fmt::{Display, Formatter};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSettings {
    /// How long the responses can/should be cached
    time_to_live: Duration,
    /// How long the files should be cached before checking
    /// for updates
    ///
    /// this does not necessarily mean that the file will
    /// be downloaded again, it just checks the modified time
    /// on the remote against the local file
    cache_time: Duration,
}

impl SyncSettings {
    pub fn new(time_to_live: Duration, cache_time: Duration) -> Self {
        Self {
            time_to_live,
            cache_time,
        }
    }
    // pub fn from_path(path: &Path)-> Self{
    //     let s = Self{
    //         time_to_live: Duration::from_secs(60),
    //         cache_time: None,
    //     };
    //     s
    // }
}

// region getters
impl SyncSettings {
    pub fn time_to_live(&self) -> Duration {
        self.time_to_live
    }
    pub fn cache_time(&self) -> Duration {
        self.cache_time
    }
}

// endregion
impl Display for SyncSettings {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyncSettings {{ ttl: {}s, cache_time: {}s }}",
               self.time_to_live.as_secs(),
               self.cache_time.as_secs())
    }
}
