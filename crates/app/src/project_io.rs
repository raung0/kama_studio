use std::path::Path;

use anyhow::Result;

use crate::project::{MediaId, Project};

pub fn load(path: &Path) -> Result<Project> {
    Project::load(path)
}

pub fn save(project: &Project, path: &Path) -> Result<()> {
    project.save(path)
}

pub fn import_media(project: &mut Project, path: &Path) -> Result<MediaId> {
    project.import_media(path)
}
