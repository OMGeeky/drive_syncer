use std::fmt::Debug;

use tokio::runtime::Handle;
use tracing::trace;

/// Run a future to completion on the current thread.
/// This is useful when you want to run a future in a blocking context.
/// This function will block the current thread until the provided future has run to completion.
///
/// # Be careful with deadlocks
pub fn run_async_blocking<T>(f: impl std::future::Future<Output=T> + Sized) -> T
    where T: Debug {
    trace!("run_async");
    let handle = Handle::current();
    let _enter_guard = handle.enter();
    trace!("run_async: entered handle");
    let result = futures::executor::block_on(f);
    trace!("run_async: got result: {:?}", result);
    result
}
