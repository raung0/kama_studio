use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::project::{MediaId, Project};

pub(crate) fn load(path: &Path) -> Result<Project> {
    Project::load(path)
}

pub(crate) fn save(project: &Project, path: &Path) -> Result<()> {
    project.save(path)
}

pub(crate) fn import_media(project: &mut Project, path: PathBuf) -> Result<MediaId> {
    project.import_media(path)
}
