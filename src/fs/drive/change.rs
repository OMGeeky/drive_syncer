use anyhow::{anyhow, Context};
use drive3::api::{Drive, File};
use drive3::chrono::{DateTime, Utc};
use google_drive3::api::Change as DriveChange;
use tracing::{error, instrument};

use crate::google_drive::DriveId;

#[derive(Debug)]
pub enum ChangeType {
    Drive(Drive),
    File(File),
    Removed,
}

impl ChangeType {
    fn from_drive_change(
        change_type: Option<String>,
        file: Option<File>,
        drive: Option<Drive>,
        removed: bool,
    ) -> anyhow::Result<ChangeType> {
        if removed {
            return Ok(Self::Removed);
        }
        if let Some(change_type) = change_type {
            match change_type.as_str() {
                "drive" => Ok(Self::Drive(
                    drive.context("no drive but change type was drive")?,
                )),
                "file" => Ok(Self::File(
                    file.context("no file but change type was file")?,
                )),
                _ => Err(anyhow!("invalid change type: {}", change_type)),
            }
        } else {
            Err(anyhow!("change type is missing"))
        }
    }
}

#[derive(Debug)]
pub struct Change {
    pub drive_id: DriveId,
    pub kind: ChangeType,
    pub time: DateTime<Utc>,
    pub removed: bool,
}

impl TryFrom<DriveChange> for Change {
    type Error = anyhow::Error;
    #[instrument]
    fn try_from(drive_change: DriveChange) -> anyhow::Result<Self> {
        let removed = drive_change.removed.unwrap_or(false);
        let drive_id = drive_change.file_id.context("drive_id is missing");
        if let Err(e) = drive_id {
            error!("drive_id is missing: {:?}", e);
            return Err(anyhow!("drive_id is missing: {:?}", e));
        }
        Ok(Self {
            drive_id: DriveId::from(drive_id?),
            kind: ChangeType::from_drive_change(
                drive_change.change_type,
                drive_change.file,
                drive_change.drive,
                removed,
            )?,
            time: drive_change.time.context("time is missing")?,
            removed,
        })
    }
}
