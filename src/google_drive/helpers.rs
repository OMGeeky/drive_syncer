use std::str::FromStr;

use google_drive3::api::File;
use mime::Mime;

pub fn get_mime_from_file_metadata(file: &File) -> anyhow::Result<Mime> {
    Ok(Mime::from_str(
        &file.mime_type.as_ref().unwrap_or(&"*/*".to_string()),
    )?)
}
