use std::ffi::{OsStr, OsString};
use std::fmt::{Debug, Display};
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use google_drive3::api::{Change, File, Scope, StartPageToken};
use google_drive3::hyper::client::HttpConnector;
use google_drive3::hyper::{Body, Response};
use google_drive3::hyper_rustls::HttpsConnector;
use google_drive3::DriveHub;
use google_drive3::{hyper_rustls, oauth2};
use hyper::Client;
use tokio::fs;
use tracing::{debug, error, instrument, trace, warn};

use crate::google_drive::{helpers, DriveId};
use crate::prelude::*;

const FIELDS_FILE: &str = "id, name, size, mimeType, kind, md5Checksum, parents, trashed, createdTime, modifiedTime, viewedByMeTime";

#[derive(Clone)]
pub struct GoogleDrive {
    hub: DriveHub<HttpsConnector<HttpConnector>>,
}

impl GoogleDrive {
    #[instrument]
    pub(crate) async fn list_all_files(&self) -> Result<Vec<File>> {
        let mut files = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            debug!("list_files: page_token: {:?}", page_token);
            let mut request = self
                .hub
                .files()
                .list()
                .q("trashed = false and 'me' in owners") //gets only own files and files not in the trash bin
                .param("fields", &format!("nextPageToken, files({})", FIELDS_FILE));
            if let Some(page_token) = page_token {
                request = request.page_token(&page_token);
            }
            let (_response, result) = request.doit().await?;
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

impl GoogleDrive {
    #[instrument]
    pub(crate) async fn get_start_page_token(&self) -> Result<StartPageToken> {
        let (_response, start_page_token) =
            self.hub.changes().get_start_page_token().doit().await?;
        Ok(start_page_token)
    }
}

impl GoogleDrive {
    #[instrument]
    pub(crate) async fn get_changes_since(
        &self,
        start_page_token: &mut StartPageToken,
    ) -> Result<Vec<Change>> {
        let mut changes = vec![];
        let mut page_token: Option<String> = None;
        loop {
            debug!(
                "getting changes since {:?} page: {:?}",
                start_page_token, page_token
            );
            let file_spec = &format!("file({})", FIELDS_FILE);
            let mut request = self
                .hub
                .changes()
                .list(
                    &start_page_token
                        .start_page_token
                        .as_ref()
                        .context("no start_page_token")?,
                )
                .param(
                    "fields",
                    &format!(
                        "changes({}, changeType, removed, fileId, driveId, drive, time),\
                         newStartPageToken, nextPageToken",
                        file_spec
                    ),
                );
            if let Some(page_token) = &page_token {
                request = request.page_token(page_token);
            }
            let response = request.doit().await.context("could not get changes");
            if let Err(e) = &response {
                error!("error getting changes: {:?}", e);
                return Err(anyhow!("error getting changes: {:?}", e));
            }
            let (_response, change_list) = response?;
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
    pub(crate) async fn get_metadata_for_file(&self, drive_id: DriveId) -> Result<File> {
        let drive_id = drive_id.to_string();
        let (_response, file) = self
            .hub
            .files()
            .get(&drive_id)
            .param("fields", &FIELDS_FILE)
            .doit()
            .await?;

        Ok(file)
    }
}

impl GoogleDrive {
    #[instrument(skip(file), fields(file_name = file.name, file_id = file.drive_id))]
    pub async fn upload_file_content_from_path(&self, file: File, path: &Path) -> Result<()> {
        update_file_content_on_drive_from_path(&self, file, path).await?;
        Ok(())
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
            Err(_) => return Err(anyhow!("invalid path")),
        };
        let parent_drive_id: OsString = match parent_drive_id {
            Some(parent_drive_id) => parent_drive_id,
            None => DriveId::from("root"),
        }
        .into();
        let parent_drive_id = match parent_drive_id.into_string() {
            Ok(parent_drive_id) => parent_drive_id,
            Err(_) => return Err(anyhow!("invalid parent_drive_id")),
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
        let (_response, files) = match req {
            Ok((response, files)) => (response, files),
            Err(e) => {
                warn!("get_id: Error: {}", e);
                return Err(anyhow!("Error"));
            }
        };

        if files.files.is_none() {
            warn!("get_id: No files found (0)");
            return Err(anyhow!("No files found"));
        }
        let files = files.files.unwrap();
        if files.len() == 0 {
            warn!("get_id: No files found (1)");
            return Err(anyhow!("No files found"));
        }
        if files.len() > 1 {
            warn!("get_id: Multiple files found");
            return Err(anyhow!("Multiple files found"));
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
        let auth = oauth2::read_application_secret("auth/client_secret.json").await?;

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

        let drive = GoogleDrive { hub };
        Ok(drive)
    }
    #[instrument]
    pub async fn list_files(&self, folder_id: DriveId) -> Result<Vec<File>> {
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
            let (_response, result) = self
                .hub
                .files()
                .list()
                .param("fields", &format!("nextPageToken, files({})", FIELDS_FILE))
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
        .ok_or(anyhow!("hello_world.txt not found"))?;
    debug!("hello_world_file: id:{:?}", hello_world_file.id);
    let target_path = "/tmp/hello_world.txt";
    let target_path = Path::new(target_path);
    // download_file(&mut drive, hello_world_file, target_path).await?;
    debug!("target_path: {:?}", target_path);
    debug!("download_file_by_id");
    let hello_world_file_id = hello_world_file.id.as_ref().ok_or(anyhow!(""))?;
    download_file_by_id(&mut drive, hello_world_file_id, target_path).await?;
    debug!("get_file_header_by_id");
    get_file_header_by_id(&mut drive, hello_world_file_id).await?;
    debug!("done");
    Ok(())
}

async fn download_file_by_id(
    hub: &GoogleDrive,
    id: impl Into<String>,
    target_path: &Path,
) -> Result<File> {
    let id = id.into();
    let (response, content): (Response<Body>, File) = hub
        .hub
        .files()
        .get(&id)
        .add_scope(Scope::Readonly)
        .acknowledge_abuse(true)
        .param("alt", "media")
        .doit()
        .await?;

    debug!("download_file_by_id(): response: {:?}", response);
    debug!("download_file_by_id(): content: {:?}", content);
    write_body_to_file(response, target_path).await?;
    let (_, file) = hub
        .hub
        .files()
        .get(&id)
        .add_scope(Scope::Readonly)
        .param("fields", FIELDS_FILE)
        .doit()
        .await?;
    debug!("download_file_by_id(): file: {:?}", file);

    Ok(file)
}

async fn write_body_to_file(response: Response<Body>, target_path: &Path) -> Result<()> {
    use futures::StreamExt;
    debug!("write_body_to_file(): target_path: {:?}", target_path);

    let mut file = std::fs::File::create(target_path)?;

    let mut stream = response.into_body();
    let _buffer = bytes::BytesMut::new();
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
    let (_response, content) = hub.hub.files().get(id).doit().await?;

    Ok(content)
}

async fn get_files_by_name(drive: &GoogleDrive, name: impl Into<String>) -> Result<Vec<File>> {
    let name = name.into();
    if name.is_empty() {
        return Err(anyhow!("name cannot be empty"));
    }
    if name.contains("'") {
        return Err(anyhow!("name cannot contain single quote"));
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
    let files: Vec<File> = files.files.unwrap_or(vec![]);
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
    let files: Vec<File> = hello_world_list.files.unwrap_or(vec![]);
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

pub async fn create_file_on_drive_from_path(
    drive: &GoogleDrive,
    file: File,
    path: &Path,
    mime_type: mime::Mime,
) -> Result<()> {
    let content = fs::File::open(path).await?;
    create_file_on_drive(drive, file, mime_type, content).await?;
    Ok(())
}

pub async fn create_file_on_drive(
    drive: &GoogleDrive,
    file: File,
    mime_type: mime::Mime,
    content: fs::File,
) -> Result<File> {
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
    file: File,
    source_path: &Path,
) -> Result<()> {
    debug!(
        "update_file_content_on_drive_from_path(): source_path: {:?}",
        source_path
    );
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
    mut file: File,
    content: fs::File,
) -> Result<()> {
    let stream = content.into_std().await;
    let mime_type = helpers::get_mime_from_file_metadata(&file)?;
    let id = file
        .id
        .clone()
        .context(format!("file metadata has no drive id: {:?}", file))?;
    //remove unchangeable data from metadata (that I still need in this request, the rest should only be the changes)
    file.id = None;
    file.mime_type = None;
    debug!("starting upload");
    let (response, file) = drive
        .hub
        .files()
        .update(file, &id)
        .upload_resumable(stream, mime_type)
        .await?;
    debug!("upload done!");
    debug!("update_file_on_drive(): response: {:?}", response);
    debug!("update_file_on_drive(): file: {:?}", file);
    Ok(())
}
