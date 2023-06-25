# TODOs

### Priorities:
- critical: this is critical and needs to be fixed asap as it creates extreme problems like crashes, build errors or data loss regularly
- 1: this is needed before it can be released
- 2: this is important but not critical/not necessary for a release
- 3: nice to have

### Prio critical:

- 

### Prio 1:
- start parameters
- settings
- fully implement the changes api to check for changes when accessing files
- file operations:
  - create
  - delete
  - move/rename
    - works for inside the mount
    - needs work for moving out of/into mount folder
      - it just can't find the target folder if I try to move a file out of the drive
      - same for moving into the drive folder
      - basically files have to be created/deleted on the drive if files are moved into/out of the drive folder
        - in that case the data also has to be moved over to the target location (actual move, not just a parent id change)
- implement notifying the user of needed actions like conflicts or required authentications via the cli 

### Prio 2:
- use the directories crate to get locations where to store configs, cached files etc
- up-/download ignore files
- offline files list (gitignore style?)
- cli way of adding/removing ignores and offline files
- support for multiple drives (maybe by running naming each drive, then the user can run two terminals with each drive if needed?)


### Prio 3:

- maybe also let the user decide if he wants notifications via the [notify-rust](https://docs.rs/notify-rust/latest/notify_rust/#example-3-ask-the-user-to-do-something) crate as a gui notification or 
  just wants to stay in the CLI (start param?)
- gui way of adding/removing ignores and offline files
- maybe global and drive specific settings and ignores (per user and per drive)
  - like a setting for all drives and a setting for each drive that is connected
- make all permissions for Google-Drive optional on each request (maybe except the one that is currently needed for that action)
  - like how you can choose to not give it write permissions but if you want to, 
    you could, without the client to have to send another request, requesting write permissions
  - should be some checkboxes or something, but idk if yup supports that







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