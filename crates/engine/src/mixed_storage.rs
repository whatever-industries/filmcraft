//! Opt-in native session with desktop autosave preferences and document-scoped journals.
use crate::{
    EngineError, Result, Session,
    autosave::{self, AutosaveConfig, Persistence},
};
use serde_json::{Value, json};

impl Session {
    /// Headless entry point for the single-editor desktop storage lifecycle.
    pub fn local_project_editor(config: AutosaveConfig) -> Result<Self> {
        let mut session = Self::local_folder_editor();
        session.enable_folder_recovery(&config.data_dir)?;
        session.start_autosave(config.clone()).map_err(|e| EngineError::Other(e.to_string()))?;
        session.mixed_config = Some(config);
        Ok(session)
    }

    /// Called after validating the destination and immediately before replacing the document.
    /// Each relinquished standard writer is joined, including any in-flight autosave. Its
    /// queued acknowledgements cannot be delivered through the replacement worker.
    pub(super) fn prepare_document_switch(&mut self) -> Result<()> {
        if self.editing_bins() {
            self.save_open_bins()?;
            return Ok(());
        }
        if self.editing_folder.is_some() {
            self.flush_folder_recovery()?;
            return Ok(());
        }
        let Some(config) = self.mixed_config.clone() else { return Ok(()) };
        self.sync_persistence();
        let old = self.persistence.as_ref().ok_or_else(|| EngineError::Other("standard project journal unavailable".into()))?;
        let confirmed = old.flush().map_err(EngineError::Other)?;
        if self.is_dirty() && confirmed != Some(self.revision) {
            return Err(EngineError::Other("outgoing project checkpoint did not match the current edit".into()));
        }
        // Allocate before relinquishing the outgoing worker so failures leave it usable.
        let next = Persistence::start(&config, self.prefs.auto_save.clone()).map_err(|e| EngineError::Other(e.to_string()))?;
        if let Some(old) = self.persistence.replace(next) {
            old.close();
        }
        self.refresh_standard_candidates();
        Ok(())
    }

    fn refresh_standard_candidates(&mut self) {
        if let Some(p) = self.persistence.as_mut() {
            p.candidates = autosave::scan_candidates(&p.data_dir, Some(&p.session_dir));
        }
    }

    pub(super) fn folder_autosave_active(&self) -> bool {
        self.folder_recovery.is_some() && (self.mixed_config.is_none() || self.editing_folder.is_some())
    }

    pub(super) fn mixed_candidates(&mut self) -> Result<Vec<Value>> {
        self.refresh_standard_candidates();
        let mut rows = Vec::new();
        for (kind, list) in [
            ("projectFile", self.persistence.as_ref().map(Persistence::candidates_json).unwrap_or_else(|| json!([]))),
            ("projectFolder", self.folder_candidates()?),
        ] {
            if let Value::Array(entries) = list {
                for mut entry in entries {
                    entry["storage"] = kind.into();
                    rows.push(entry);
                }
            }
        }
        Ok(rows)
    }

    pub(super) fn use_folder_candidate(&mut self, params: &Value, discard: bool) -> Result<bool> {
        if self.mixed_config.is_none() {
            return Ok(self.folder_recovery.is_some());
        }
        let entries = self.mixed_candidates()?;
        let files = entries.iter().any(|e| e["storage"] == "projectFile");
        let folders = entries.iter().any(|e| e["storage"] == "projectFolder");
        if params.get("id").is_some_and(|id| !id.is_string()) {
            return Err(EngineError::Other("expected a recovery candidate id".into()));
        }
        let bulk = discard && params["all"] == true;
        if self.supports_desktop_bins() && entries.len() > 1 && !bulk && !params["id"].is_string() {
            return Err(EngineError::Other("choose one recovery candidate by storage and id".into()));
        }
        if files && folders && (!params["storage"].is_string() || (!bulk && !params["id"].is_string())) {
            return Err(EngineError::Other("choose a recovery candidate by storage (projectFile or projectFolder) and id".into()));
        }
        if let (Some(kind), Some(id)) = (params.get("storage"), params.get("id"))
            && !entries.iter().any(|row| &row["storage"] == kind && &row["id"] == id)
        {
            return Err(EngineError::Other("no matching candidate in the selected recovery store".into()));
        }
        match params.get("storage") {
            Some(kind) if kind == "projectFolder" && folders => Ok(true),
            Some(kind) if kind == "projectFile" && files => Ok(false),
            Some(_) => Err(EngineError::Other("unknown recovery store or no matching candidate".into())),
            None => Ok(folders),
        }
    }
}

#[cfg(test)]
mod tests;
