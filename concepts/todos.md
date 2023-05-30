# TODOs

### Priorities:
- critical: this is critical and needs to be fixed asap as it creates extreme problems like crashes or data loss regularly
- 1: this is needed before it can be released
- 2: this is important but not critical/not necessary for a release
- 3: nice to have

### Prio critical:

- 

### Prio 1:
- start parameters
- settings
- implement the changes api to check for changes when accessing files
- file operations:
  - create
  - delete
  - move/rename

### Prio 2:
- implement notifying the user of needed actions like conflicts or required authentications via the [notify-rust](https://docs.rs/notify-rust/latest/notify_rust/#example-3-ask-the-user-to-do-something) crate
  - maybe also let the user decide if he wants notifications like this or just wants to stay in the CLI (start param?)


### Prio 3:












## Done TODOs:


### Prio critical:
- fix freezing
  - freeze happens after a few requests. I have no Idea why but run_async_blocking never
    returns sometimes for some reason.
  - it always happens after adding about 7-8 times a character to the end of a string causing a lookup
    - example going from ``cat /tmp/fuse/3/sample_folder/hello_a`` to
      ``cat /tmp/fuse/3/sample_folder/hello_aaaaaaaaa``
    - the lookups don't have to be successful (im not sure if they need to fail to freeze the system)
  - when looking at the tokio-console it shows 4 tasks and 4 resources after start, after
    each character added it jumps up about 8 resources to 12 but then goes back down to 4
    after a few seconds for the first 7 to 8 attempts. When it hangs up it only goes down to 5,
    sometimes 6 resources, not 4.
  - => DONE
  - This issue was solved with the tokio [blocking_recv](https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.Receiver.html#method.blocking_recv)
    and [blocking_send](https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.Sender.html#method.blocking_send)


### Prio 1:

- 

### Prio 2:

- 

### Prio 3:

- 