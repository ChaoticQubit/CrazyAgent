use super::*;
use crate::setup::SetupStatus;

impl OwnerController {
    /// Reads what setup shows: each tool CrazyAgent needs, each default-pack entry
    /// and whether a setup run is going on.
    /// Takes nothing. Returns the status.
    pub async fn setup_status(&mut self) -> Result<SetupStatus, EngineError> {
        match self.read(OwnerRead::SetupStatus).await?.data {
            OwnerData::SetupStatus { status } => Ok(status),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Installs the missing tools and the ticked default-pack entries; the ticked
    /// MCP servers count as approved by the owner.
    /// Takes the ticked entry ids and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn run_setup(
        &mut self,
        pack: Vec<String>,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.setup_status().await?;
        self.begin_mutation(OwnerMutation::RunSetup { pack }, generation)?;
        self.send_pending().await
    }

    /// Turns one installed default-pack entry on or off for every project.
    /// Takes the entry id, on or off and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn set_pack_entry(
        &mut self,
        id: String,
        on: bool,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.setup_status().await?;
        self.begin_mutation(OwnerMutation::SetPackEntry { id, on }, generation)?;
        self.send_pending().await
    }

    /// Removes one installed default-pack entry; an always-on entry is refused.
    /// Takes the entry id and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn remove_pack_entry(
        &mut self,
        id: String,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.setup_status().await?;
        self.begin_mutation(OwnerMutation::RemovePackEntry { id }, generation)?;
        self.send_pending().await
    }

    /// Moves every project folder from the current projects home into another
    /// folder and makes that folder the home.
    /// Takes the absolute folder path and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn move_projects_home(
        &mut self,
        path: String,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.projects_home().await?;
        self.begin_mutation(
            OwnerMutation::MoveProjectsHome {
                path,
                move_projects: true,
            },
            generation,
        )?;
        self.send_pending().await
    }
}
