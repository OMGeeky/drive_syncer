# thing to consider:
- [ ] How to prevent the file explorer to automatically generate thumbnails for the files
  - DT_UNKNOWN in readdir should do the trick
    - in linux can a fuse filesystem notify the file explorer that it does not support thumbnails?
      - Yes, a FUSE filesystem can notify the file explorer that it does not support thumbnails. This can be achieved by implementing the readdir function in the FUSE filesystem and setting the d_type field of the dirent struct to DT_UNKNOWN for files that do not support thumbnails. By doing this, the file explorer will not attempt to generate a thumbnail for the file.
    - does this have any implications other than not generating thumbnails for example how a file is opened?
      - No, setting `d_type` to `DT_UNKNOWN` for files in a FUSE filesystem that do not support thumbnails should not have any implications other than not generating thumbnails. The `d_type` field of the `dirent` struct is used by the file explorer to determine the type of the file. If `d_type` is set to `DT_UNKNOWN`, the file explorer will not be able to determine the file type and will not use it to make any decisions about how to open the file.
      - According to the `readdir` man page, the `d_type` field is not specified in POSIX.1 and is not present on all systems. It is an unstandardized field that is mainly available on BSD systems and some Linux filesystems like Btrfs, ext2, ext3, and ext4. If a filesystem does not fill `d_type` properly, all applications must properly handle a return of `DT_UNKNOWN`. Therefore, it is safe to set `d_type` to `DT_UNKNOWN` for files that do not support thumbnails in a FUSE filesystem.
      - It is also worth noting that FUSE provides two ways to identify the file being operated upon: the `path` argument and the `file handle` in the `fuse_file_info` structure. The `path` argument is always available, but pathname lookup can be expensive.


# Things I need to implement with the file provider




## release

`release`: This function is called when there are no more references to an open file,
which means all file descriptors are closed and all memory mappings are unmapped. For
every `open` call, there will be exactly one `release` call. The purpose of this function
is to clean up any resources associated with the open file and perform any finalization
tasks before the file is considered closed. Note that error values returned by `release`
are not propagated to the `close()` or `munmap()` system calls that triggered the release.

```rust
fn release(&mut self, _req: &Request<'_>, ino: u64, fh: u64, flags: u32, lock_owner: u64, flush: bool, reply: ReplyEmpty)
```

## fsync

`fsync`: This function is called to synchronize a file's in-memory state with the storage
device. It ensures that any pending writes are flushed to the storage device and that the
file data is consistent. The `fsync` function takes a parameter `datasync` that indicates
whether only the file data should be flushed (`true`) or both file data and metadata should
be flushed (`false`).

```rust
fn fsync(&mut self, _req: &Request<'_>, ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty)
```

## open

`open`: The `open` function is called when a file is opened in the filesystem. The main
purpose of this function is to check if the operation is permitted for the given flags
and return success (0) if the file can be opened. Optionally, a file handle may be
returned, which will be passed to subsequent read, write, flush, fsync, and release calls.
It is important to note that no creation or truncation flags (O_CREAT, O_EXCL, O_TRUNC)
will be passed to the `open` function. The filesystem implementation should only check if
the operation is allowed based on the provided flags [Source 2](https://metacpan.org/pod/Fuse).

```rust
fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen)
```

## flush

`flush`: The `flush` function is called to synchronize any cached data before the file is
closed. It may be called multiple times before a file is closed. Note that this function
is not equivalent to `fsync()` and it's not a request to sync dirty data. It is called on
each `close()` of a file descriptor, as opposed to `release`, which is called on the close
of the last file descriptor for a file. Under Linux, errors returned by `flush()` will be
passed to userspace as errors from `close()`. However, many applications ignore errors on
`close()`, and on non-Linux systems, `close()` may succeed even if `flush()` returns an
error. For these reasons, filesystems should not assume that errors returned by `flush`
will ever be noticed or even delivered
[Source 4](https://libfuse.github.io/doxygen/structfuse__operations.html).

```rust
fn flush(&mut self, _req: &Request<'_>, ino: u64, fh: u64, lock_owner: u64, reply: ReplyEmpty)
```

## write

`write`: The `write` function is called when a user process wants to write data to a file
in the filesystem. The main purpose of this function is to write the given data at the
specified offset in the file, and return the number of bytes successfully written. The
function should handle partial writes and update the file size if necessary. The write
operation should respect the file's open mode (e.g., O_WRONLY or O_RDWR) and any file locks.

```rust
fn write(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, data: Vec<u8>, flags: i32, reply: ReplyWrite)
```

## read

`read`: The `read` function is called when a user process wants to read data from a file
in the filesystem. The main purpose of this function is to read the specified number of
bytes from the file starting at the given offset and return the data to the caller. The
function should handle partial reads and return an appropriate amount of data if the
requested size exceeds the file's remaining size from the specified offset. The read
operation should respect the file's open mode (e.g., O_RDONLY or O_RDWR) and any file locks.

```rust
fn read(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, size: u32, reply: ReplyData)
```

_________________________________________________________

# Some others I should probably check up on:

## getattr

`getattr`: This function is called to get the attributes of a file or directory, such as
its size, creation time, owner, and permissions. The filesystem implementation should
look up the attributes for the specified inode number and return them in a `FileAttr` structure.

```rust
fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr)
```

## readdir

`readdir`: This function is called when a user process wants to list the contents of a
directory. The filesystem implementation should return the list of entries in the
specified directory, including files, directories, and symbolic links.

```rust
fn readdir(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, mut reply: ReplyDirectory)
```

## mkdir

`mkdir`: This function is called when a user process wants to create a new directory.
The filesystem implementation should create the new directory with the specified name,
mode, and parent directory.

```rust
fn mkdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, mode: u32, reply: ReplyEntry)
```

## rmdir

`rmdir`: This function is called when a user process wants to remove a directory. The
filesystem implementation should remove the specified directory if it is empty.

```rust
fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty)
```

## create

`create`: This function is called when a user process wants to create a new file. The
filesystem implementation should create the new file with the specified name, mode, and parent directory, and return a
file handle.

```rust
fn create(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, mode: u32, flags: i32, reply: ReplyCreate)
```

## unlink

`unlink`: This function is called when a user process wants to remove a file. The filesystem
implementation should remove the specified file.

```rust
fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty)
```

## rename

`rename`: This function is called when a user process wants to rename a file or directory.
The filesystem implementation should move the specified file or directory from its current
location to the new location, updating the parent directory as needed.

```rust
fn rename(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr, reply: ReplyEmpty)
```
