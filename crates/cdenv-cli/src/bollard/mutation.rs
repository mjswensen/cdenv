//! Docker resource mutation and guarded cleanup operations.

use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::{
    RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder, RenameContainerOptionsBuilder,
    StopContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use cdenv_core::{ContainerId, GenerationId, InstallationId, ProfileId, WorkspaceName};

use super::inspection::{ContainerExpectation, verify_label, verify_value};
use super::{
    BollardAdapter, BollardAdapterError, BollardApi, BollardApiError, BollardApiRequest,
    BollardApiResponse, GENERATED_IMAGE_LABEL, GENERATION_LABEL, INSTALLATION_LABEL, PROFILE_LABEL,
    WORKSPACE_LABEL,
};
use crate::ImageId;

pub(super) async fn execute_start_container(
    client: &Docker,
    id: &str,
) -> Result<BollardApiResponse, BollardApiError> {
    client
        .start_container(id, None)
        .await
        .map(|()| BollardApiResponse::Unit)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_stop_container(
    client: &Docker,
    id: &str,
    seconds: i32,
) -> Result<BollardApiResponse, BollardApiError> {
    let options = StopContainerOptionsBuilder::new().t(seconds).build();
    client
        .stop_container(id, Some(options))
        .await
        .map(|()| BollardApiResponse::Unit)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_rename_container(
    client: &Docker,
    id: &str,
    name: &str,
) -> Result<BollardApiResponse, BollardApiError> {
    let options = RenameContainerOptionsBuilder::new().name(name).build();
    client
        .rename_container(id, options)
        .await
        .map(|()| BollardApiResponse::Unit)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_upload_archive(
    client: &Docker,
    id: &str,
    path: &str,
    archive: Vec<u8>,
) -> Result<BollardApiResponse, BollardApiError> {
    let options = UploadToContainerOptionsBuilder::new().path(path).build();
    client
        .upload_to_container(id, Some(options), bollard::body_full(archive.into()))
        .await
        .map(|()| BollardApiResponse::Unit)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_remove_container(
    client: &Docker,
    id: &str,
) -> Result<BollardApiResponse, BollardApiError> {
    let options = RemoveContainerOptionsBuilder::new()
        .force(false)
        .v(false)
        .build();
    client
        .remove_container(id, Some(options))
        .await
        .map(|()| BollardApiResponse::Unit)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_remove_image(
    client: &Docker,
    id: &str,
) -> Result<BollardApiResponse, BollardApiError> {
    let options = RemoveImageOptionsBuilder::new()
        .force(false)
        .noprune(true)
        .build();
    client
        .remove_image(id, Some(options), None)
        .await
        .map(|_| BollardApiResponse::Unit)
        .map_err(BollardApiError::from_bollard)
}

/// Identity required before narrowly scoped image cleanup.
#[derive(Clone, Copy, Debug)]
pub struct ImageCleanupExpectation<'a> {
    /// Exact operation-claimed image ID.
    pub id: &'a ImageId,
    /// Installation namespace.
    pub installation: &'a InstallationId,
    /// Workspace identity.
    pub workspace: &'a WorkspaceName,
    /// Environment generation.
    pub generation: GenerationId,
    /// Compatibility profile.
    pub profile: &'a ProfileId,
}

impl<A: BollardApi> BollardAdapter<A> {
    /// Starts one exact container.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, or response-shape error.
    pub async fn start(&self, id: &ContainerId) -> Result<(), BollardAdapterError> {
        self.unit(
            "start container",
            BollardApiRequest::StartContainer { id: id.to_string() },
        )
        .await
    }

    /// Stops one exact container with Docker's graceful-stop timeout.
    ///
    /// # Errors
    ///
    /// Returns an invalid timeout, API, timeout, or response-shape error.
    pub async fn stop(&self, id: &ContainerId, grace: Duration) -> Result<(), BollardAdapterError> {
        let seconds =
            i32::try_from(grace.as_secs()).map_err(|_| BollardAdapterError::InvalidStopTimeout)?;
        self.unit(
            "stop container",
            BollardApiRequest::StopContainer {
                id: id.to_string(),
                seconds,
            },
        )
        .await
    }

    /// Renames one exact container.
    ///
    /// # Errors
    ///
    /// Returns an invalid name, API, timeout, or response-shape error.
    pub async fn rename(&self, id: &ContainerId, name: &str) -> Result<(), BollardAdapterError> {
        if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_control) {
            return Err(BollardAdapterError::InvalidContainerName);
        }
        self.unit(
            "rename container",
            BollardApiRequest::RenameContainer {
                id: id.to_string(),
                name: name.to_owned(),
            },
        )
        .await
    }

    /// Uploads an uncompressed tar archive for extraction below an absolute container path.
    ///
    /// # Errors
    ///
    /// Returns an invalid path, API, timeout, or response-shape error.
    pub async fn upload_archive(
        &self,
        id: &ContainerId,
        path: &str,
        archive: &[u8],
    ) -> Result<(), BollardAdapterError> {
        if !path.starts_with('/') || path.contains('\0') {
            return Err(BollardAdapterError::InvalidArchivePath);
        }
        self.unit(
            "upload archive",
            BollardApiRequest::UploadArchive {
                id: id.to_string(),
                path: path.to_owned(),
                archive: archive.to_vec(),
            },
        )
        .await
    }

    /// Removes a container only after re-inspecting its exact ID and all cdenv identity labels.
    ///
    /// # Errors
    ///
    /// Returns an inspect/verification failure, a running-state refusal, or a removal failure.
    pub async fn cleanup_container(
        &self,
        expected: ContainerExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        let inspection = self.inspect_container(expected.id).await?;
        Self::verify_container(&inspection, expected)?;
        if inspection.running {
            return Err(BollardAdapterError::CleanupRunningContainer { id: inspection.id });
        }
        self.unit(
            "remove container",
            BollardApiRequest::RemoveContainer {
                id: inspection.id.to_string(),
            },
        )
        .await
    }

    /// Removes an image only after exact ID and cdenv-generated labels are verified.
    ///
    /// # Errors
    ///
    /// Returns an inspect/verification or removal failure.
    pub async fn cleanup_image(
        &self,
        expected: ImageCleanupExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        let inspection = self.inspect_image(expected.id.as_str()).await?;
        verify_value("image ID", expected.id.as_str(), inspection.id.as_str())?;
        verify_label(
            &inspection.labels,
            INSTALLATION_LABEL,
            expected.installation.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            WORKSPACE_LABEL,
            expected.workspace.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            GENERATION_LABEL,
            &expected.generation.to_string(),
        )?;
        verify_label(&inspection.labels, PROFILE_LABEL, expected.profile.as_str())?;
        verify_label(&inspection.labels, GENERATED_IMAGE_LABEL, "true")?;
        self.unit(
            "remove image",
            BollardApiRequest::RemoveImage {
                id: inspection.id.as_str().to_owned(),
            },
        )
        .await
    }
}
