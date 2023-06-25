# drive_syncer

this is a cli program very early in development, targeting to create an
easy-to-use replacement of the official windows Google Drive client.

Features:

| title                                                                      | status         |
|----------------------------------------------------------------------------|----------------|
| streaming files                                                            | Mostly Working |
| local file caching                                                         | WIP            |
| offline available files                                                    | Planned        |
| setting local file permissions (like execute, etc) for each file/dir       | WIP            |
| upload/download filters in form of gitignore like file (maybe gui support) | Planned        |
| keeping local & remote in sync (even with offline available files)         | WIP            |

## this is still very early in development and definitely **not** ready for normal use yet, it will probably destroy some files irreversibly in its current state.

#### Suggestions, Tips and PRs are very welcome!



## Remarks

- This library does not really support multiple parents for a single file 
  and probably will remove existing relationships to other parents if the 
  file is moved locally
