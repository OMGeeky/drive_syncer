use std::ffi::{OsStr, OsString};
use std::fmt::{Debug, Display, Error};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

// use drive3::api::Scope::File;
use anyhow::{anyhow, Context};
use drive3::{hyper_rustls, oauth2};
use drive3::api::{Change, File, Scope, StartPageToken};
use drive3::chrono::{DateTime, Utc};
use drive3::client::ReadSeek;
use drive3::DriveHub;
use drive3::hyper::{body, Body, Response};
use drive3::hyper::body::HttpBody;
use drive3::hyper::client::HttpConnector;
use drive3::hyper_rustls::HttpsConnector;
use futures::{Stream, StreamExt};
use hyper::Client;
use mime::{FromStrError, Mime};
use tokio::{fs, io};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;
use tracing::{debug, error, instrument, trace, warn};
use tracing::field::debug;

use crate::google_drive::{drive, DriveId, helpers};
use crate::prelude::*;

#[derive(Clone)]
pub struct GoogleDrive {
    hub: DriveHub<HttpsConnector<HttpConnector>>,
}

impl GoogleDrive {
    #[instrument]
    pub(crate) async fn get_start_page_token(&self) -> anyhow::Result<StartPageToken> {
        let (_response, start_page_token) = self.hub.changes().get_start_page_token().doit().await?;
        Ok(start_page_token)
    }
}

impl GoogleDrive {
    #[instrument]
    pub(crate) async fn get_changes_since(&self, start_page_token: &mut StartPageToken) -> anyhow::Result<Vec<Change>> {
        let mut changes = vec![];
        let mut page_token: Option<String> = None;
        loop {
            debug!("getting changes since {:?} page: {:?}", start_page_token, page_token);
            let mut request = self
                .hub
                .changes()
                .list(&start_page_token
                    .start_page_token
                    .as_ref()
                    .context("no start_page_token")?);
            if let Some(page_token) = &page_token {
                request = request.page_token(page_token);
            }
            let (_response, change_list) = request
                .doit()
                .await
                .context("could not get changes")?;
            if let Some(change_list) = change_list.changes {
                changes.extend(change_list);
            }
            if let Some(next_page_token) = change_list.next_page_token {
                page_token = Some(next_page_token);
            } else if let Some(new_start_page_token) = change_list.new_start_page_token {
                start_page_token.start_page_token = Some(new_start_page_token);
                break;
            } else {
                error!("no next_page_token or new_start_page_token");
                break;
            }
        }
        Ok(changes)
    }
}

impl GoogleDrive {
    #[instrument]
    pub(crate) async fn get_metadata_for_file(&self, drive_id: DriveId) -> anyhow::Result<File> {
        let drive_id = drive_id.into_string().map_err(|_| anyhow!("invalid drive_id"))?;
        let (response, file) = self
            .hub
            .files()
            .get(&drive_id)
            .param("fields", "id, name, modifiedTime, driveId, size, createdTime, viewedByMeTime")
            .doit().await?;

        Ok(file)
    }
}

impl GoogleDrive {
    #[instrument(skip(file), fields(file_name = file.name, file_id = file.drive_id))]
    pub async fn upload_file_content_from_path(&self, file: File, path: &Path) -> anyhow::Result<()> {
        update_file_content_on_drive_from_path(&self, file, path).await?;
        Ok(())
    }
}

impl GoogleDrive {
    pub(crate) async fn get_modified_time(&self, drive_id: DriveId) -> Result<SystemTime> {
        let drive_id: OsString = drive_id.into();
        let drive_id = drive_id.into_string().map_err(|_| anyhow!("invalid drive_id"))?;
        let (response, file) = self.hub.files().get(&drive_id).param("fields", "modifiedTime").doit().await?;
        let x = file.modified_time.ok_or_else(|| anyhow!("modified_time not found"))?;
        Ok(x.into())
    }
}

impl GoogleDrive {
    #[instrument]
    pub async fn download_file(&self, file_id: DriveId, target_file: &PathBuf) -> Result<File> {
        debug!(
            "download_file: file_id: {:50?} to {}",
            file_id,
            target_file.display()
        );
        let file_id: String = match file_id.try_into() {
            Ok(file_id) => file_id,
            Err(e) => return Err(anyhow!("invalid file_id: {:?}", e).into()),
        };

        let file = download_file_by_id(&self, file_id, target_file.as_path()).await;
        debug!("download_file: completed");
        let file = file?;

        debug!("download_file: success");

        Ok(file)
    }
}

impl GoogleDrive {
    #[instrument]
    pub async fn get_id(&self, path: &OsStr, parent_drive_id: Option<DriveId>) -> Result<DriveId> {
        debug!("Get ID of '{:?}' with parent: {:?}", path, parent_drive_id);
        let path: OsString = path.into();
        let path = match path.into_string() {
            Ok(path) => path,
            Err(_) => return Err("invalid path".into()),
        };
        let parent_drive_id: OsString = match parent_drive_id {
            Some(parent_drive_id) => parent_drive_id,
            None => DriveId::from("root"),
        }
            .into();
        let parent_drive_id = match parent_drive_id.into_string() {
            Ok(parent_drive_id) => parent_drive_id,
            Err(_) => return Err("invalid parent_drive_id".into()),
        };
        debug!("get_id: path: {}", path);
        debug!("get_id: parent_drive_id: {}", parent_drive_id);

        let req = self
            .hub
            .files()
            .list()
            .q(&format!(
                // "'{}' in parents, '{}' == name",
                "name = '{}' and '{}' in parents",
                path, parent_drive_id
            ))
            .param("fields", "files(id)")
            .doit()
            .await;
        let (response, files) = match req {
            Ok((response, files)) => (response, files),
            Err(e) => {
                warn!("get_id: Error: {}", e);
                return Err("Error".into());
            }
        };

        if files.files.is_none() {
            warn!("get_id: No files found (0)");
            return Err("No files found".into());
        }
        let files = files.files.unwrap();
        if files.len() == 0 {
            warn!("get_id: No files found (1)");
            return Err("No files found".into());
        }
        if files.len() > 1 {
            warn!("get_id: Multiple files found");
            return Err("Multiple files found".into());
        }
        let file = files.into_iter().next().unwrap();
        let id = file.id.unwrap();
        debug!("get_id: id: {}", id);
        Ok(DriveId::from(id))
    }
}

impl GoogleDrive {
    #[instrument]
    pub(crate) async fn new() -> Result<Self> {
        let auth = drive3::oauth2::read_application_secret("auth/client_secret.json").await?;

        let auth = oauth2::InstalledFlowAuthenticator::builder(
            auth,
            oauth2::InstalledFlowReturnMethod::HTTPRedirect,
        )
            .persist_tokens_to_disk("auth/tokens.json")
            .build()
            .await?;
        let http_client = Client::builder().build(
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .build(),
        );
        let hub = DriveHub::new(http_client, auth);

        let mut drive = GoogleDrive { hub };
        Ok(drive)
    }
    #[instrument]
    pub async fn list_files(&self, folder_id: DriveId) -> anyhow::Result<Vec<File>> {
        debug!("list_files: folder_id: {:?}", folder_id);
        let folder_id: OsString = folder_id.into();
        let folder_id = match folder_id.into_string() {
            Ok(folder_id) => folder_id,
            Err(_) => return Err(anyhow!("invalid folder_id")),
        };
        if folder_id.is_empty() {
            return Err(anyhow!("folder_id is empty"));
        }
        if folder_id.contains('\'') {
            return Err(anyhow!("folder_id contains invalid character"));
        }
        let mut files = Vec::new();
        let mut page_token = None;
        loop {
            debug!("list_files: page_token: {:?}", page_token);
            let (response, result) = self
                .hub
                .files()
                .list()
                .param(
                    "fields",
                    "nextPageToken, files(id, name, size, mimeType, kind)",
                )
                // .page_token(page_token.as_ref().map(String::as_str))
                .q(format!("'{}' in parents", folder_id).as_str())
                .doit()
                .await?;
            let result_files = result.files.ok_or(anyhow!("no file list returned"))?;
            debug!("list_files: response: {:?}", result_files.len());
            files.extend(result_files);
            page_token = result.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(files)
    }
}

impl Debug for GoogleDrive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GoogleDrive")
    }
}

impl Display for GoogleDrive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GoogleDrive")
    }
}

pub async fn sample() -> Result<()> {
    debug!("sample");

    let mut drive = GoogleDrive::new().await?;

    sample_list_files(&mut drive).await?;
    let hello_world_file = get_files_by_name(&mut drive, "hello_world.txt").await?;
    let hello_world_file = hello_world_file
        .first()
        .ok_or("hello_world.txt not found")?;
    debug!("hello_world_file: id:{:?}", hello_world_file.id);
    let target_path = "/tmp/hello_world.txt";
    let target_path = std::path::Path::new(target_path);
    // download_file(&mut drive, hello_world_file, target_path).await?;
    debug!("target_path: {:?}", target_path);
    debug!("download_file_by_id");
    let hello_world_file_id = hello_world_file.id.as_ref().ok_or("")?;
    download_file_by_id(&mut drive, hello_world_file_id, target_path).await?;
    debug!("get_file_header_by_id");
    get_file_header_by_id(&mut drive, hello_world_file_id).await?;
    debug!("done");
    Ok(())
}

async fn download_file(
    hub: &GoogleDrive,
    file: &drive3::api::File,
    target_path: &Path,
) -> Result<File> {
    if let Some(id) = &file.id {
        download_file_by_id(hub, id, target_path).await
    } else {
        Err("file id not found".into())
    }
}

async fn download_file_by_id(
    hub: &GoogleDrive,
    id: impl Into<String>,
    target_path: &Path,
) -> Result<File> {
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;
    let (response, content): (Response<Body>, google_drive3::api::File) = hub
        .hub
        .files()
        .get(&id.into())
        .add_scope(Scope::Readonly)
        .acknowledge_abuse(true)
        .param("alt", "media")
        .doit()
        .await?;
    //TODO: bigger files don't get downloaded. it just starts and then hangs at ~1.3MB forever
    debug!("download_file_by_id(): response: {:?}", response);
    debug!("download_file_by_id(): content: {:?}", content);
    write_body_to_file(response, target_path).await?;

    Ok(content)
}

async fn write_body_to_file(response: Response<Body>, target_path: &Path) -> Result<()> {
    use futures::StreamExt;
    debug!("write_body_to_file(): target_path: {:?}", target_path);

    let mut file = std::fs::File::create(target_path)?;

    let mut stream = response.into_body();
    let mut buffer = bytes::BytesMut::new();
    let mut counter = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        trace!("write_body_to_file(): chunk counter: {}", counter);
        file.write_all(&chunk)?;
        counter += 1;
    }
    debug!("write_body_to_file(): done");
    Ok(())
}

async fn get_file_header_by_id(hub: &GoogleDrive, id: &str) -> Result<File> {
    debug!("get_file_header_by_id(): id: {:?}", id);
    let (response, content) = hub.hub.files().get(id).doit().await?;

    Ok(content)
}

async fn get_files_by_name(
    drive: &GoogleDrive,
    name: impl Into<String>,
) -> Result<Vec<drive3::api::File>> {
    let name = name.into();
    if name.is_empty() {
        return Err("name cannot be empty".into());
    }
    if name.contains("'") {
        return Err("name cannot contain single quote".into());
    }
    let (response, files) = drive
        .hub
        .files()
        .list()
        .q(format!("name = '{}'", name).as_str())
        .doit()
        .await?;
    debug!("get_files_by_name(): response: {:?}", response);
    debug!("get_files_by_name(): files: {:?}", files);
    let files: Vec<drive3::api::File> = files.files.unwrap_or(vec![]);
    Ok(files)
}

async fn sample_list_files(drive: &GoogleDrive) -> Result<()> {
    let (hello_world_res, hello_world_list) = drive
        .hub
        .files()
        .list()
        // .q("name = 'hello_world.txt'")
        // .q("'root' in parents and trashed=false")
        .doit()
        .await?;
    debug!("hello_world_res: {:?}", hello_world_res);
    debug!("hello_world_list: {:?}", hello_world_list);
    let files: Vec<drive3::api::File> = hello_world_list.files.unwrap_or(vec![]);
    debug!("hello_world_list amount of files: {}", files.len());
    for file in files {
        let name = file.name.unwrap_or("NO NAME".to_string());
        let id = file.id.unwrap_or("NO ID".to_string());
        let kind = file.kind.unwrap_or("NO KIND".to_string());
        let mime_type = file.mime_type.unwrap_or("NO MIME TYPE".to_string());

        debug!(
            "file: {:100}name:{:100}kind: {:25}mime_type: {:100}",
            id, name, kind, mime_type
        );
    }

    Ok(())
}

async fn create_file_on_drive_from_path(
    drive: &GoogleDrive,
    file: File,
    path: &Path,
    mime_type: mime::Mime,
) -> Result<()> {
    let content = fs::File::open(path).await?;
    create_file_on_drive(drive, file, mime_type, content).await?;
    Ok(())
}

async fn create_file_on_drive(
    drive: &GoogleDrive,
    file: google_drive3::api::File,
    mime_type: mime::Mime,
    content: tokio::fs::File,
) -> Result<drive3::api::File> {
    let stream = content.into_std().await;
    let (response, file) = drive
        .hub
        .files()
        .create(file)
        .upload_resumable(stream, mime_type)
        .await?;
    debug!("create_file(): response: {:?}", response);
    debug!("create_file(): file: {:?}", file);
    Ok(file)
}

#[instrument(skip(file), fields(drive_id = file.drive_id))]
pub async fn update_file_content_on_drive_from_path(
    drive: &GoogleDrive,
    file: google_drive3::api::File,
    source_path: &Path,
) -> anyhow::Result<()> {
    debug!("update_file_content_on_drive_from_path(): source_path: {:?}", source_path);
    // {
    //     debug!("reading content from file for testing");
    //     let content = std::fs::File::open(source_path)?;
    //     let mut content = tokio::fs::File::from_std(content);
    //     let mut s = String::new();
    //     content.read_to_string(&mut s).await?;
    //     debug!("update_file_content_on_drive_from_path(): content: {:?}", s);
    // }
    let content = fs::File::open(source_path).await?;
    update_file_content_on_drive(drive, file, content).await?;
    Ok(())
}

#[instrument(skip(file, content))]
async fn update_file_content_on_drive(
    drive: &GoogleDrive,
    file: google_drive3::api::File,
    content: fs::File,
) -> anyhow::Result<()> {
    let stream = content.into_std().await;
    // let stream = content;
    let mime_type = helpers::get_mime_from_file_metadata(&file)?;
    let id = file.drive_id.clone().with_context(|| "file metadata has no drive id")?;
    debug!("starting upload");
    let (response, file) = drive
        .hub
        .files()
        .update(file, &id)
        .upload(stream, mime_type)
        .await?;
    debug!("upload done!");
    debug!("update_file_on_drive(): response: {:?}", response);
    debug!("update_file_on_drive(): file: {:?}", file);
    Ok(())
}
