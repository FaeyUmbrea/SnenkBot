use std::error::Error;
use std::path::PathBuf;

use snenk_bot::app::WorkflowStatus;
use snenk_bot::editor::{StepDestination, StepMoveDirection, StepPosition};
use snenk_bot::engine::{Event, Outcome, ScriptActivity, StepTrace};
use snenk_bot::history::RunRecord;
use snenk_bot::integration::{ConnectionStatus, ReconfigurationRequest};
use snenk_bot::schema::ConfigSchema;
use snenk_bot::workflows::WorkflowDefinition;
use snenkbot_desktop::authentication::{
    ApproveTwitchLogin, AuthenticationError, AuthenticationIdentity, StartTwitchLogin,
    TwitchAuthentication,
};
use snenkbot_desktop::commands::{
    ApplyEditorEdit, CopyTwitchLogin, CreateWorkflow, CredentialPresence, DismissError,
    EditorSaveResult, EditorSessionRequest, EditorValueSources, InputRequestIdentity,
    InspectHistory, OpenEditor, RunWorkflow, SaveEditor, SaveObsPassword, SubmitInput,
    TwitchAccountsSnapshot,
};
use snenkbot_desktop::configuration::{
    ConfigurationError, ConfigurationSaveResult, ConfigurationSessionRequest,
    ConfigurationSnapshot, OpenConfiguration, SaveConfiguration,
};
use snenkbot_desktop::editor::{EditorError, EditorSnapshot};
use snenkbot_desktop::history::{HistoryPage, HistoryQuery, HistoryQueryError};
use snenkbot_desktop::hub::{DesktopBatch, DesktopSnapshot};
use snenkbot_desktop::input::{InputBridgeError, InputEvent, InputRequest};
use specta::Types;
use specta_typescript::Typescript;

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("provide the frontend contracts directory as the first argument")?;
    export_contracts(&output)?;
    Ok(())
}

fn export_contracts(output: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let types = Types::default()
        .register::<snenkbot_desktop::choices::ActionChoices>()
        .register::<snenk_bot::choices::ConfigChoice>()
        .register::<CreateWorkflow>()
        .register::<EditorValueSources>()
        .register::<snenk_bot::value_sources::ValueSource>()
        .register::<StartTwitchLogin>()
        .register::<ApproveTwitchLogin>()
        .register::<AuthenticationIdentity>()
        .register::<AuthenticationError>()
        .register::<TwitchAuthentication>()
        .register::<TwitchAccountsSnapshot>()
        .register::<CopyTwitchLogin>()
        .register::<ConfigSchema>()
        .register::<snenk_bot::schema::ActionDefinition>()
        .register::<WorkflowDefinition>()
        .register::<StepDestination>()
        .register::<StepPosition>()
        .register::<StepMoveDirection>()
        .register::<WorkflowStatus>()
        .register::<ReconfigurationRequest>()
        .register::<ConnectionStatus>()
        .register::<DesktopSnapshot>()
        .register::<DesktopBatch>()
        .register::<InputBridgeError>()
        .register::<InputEvent>()
        .register::<InputRequest>()
        .register::<Event>()
        .register::<Outcome>()
        .register::<StepTrace>()
        .register::<ScriptActivity>()
        .register::<EditorSnapshot>()
        .register::<EditorError>()
        .register::<OpenEditor>()
        .register::<EditorSessionRequest>()
        .register::<ApplyEditorEdit>()
        .register::<SaveEditor>()
        .register::<EditorSaveResult>()
        .register::<DismissError>()
        .register::<RunWorkflow>()
        .register::<SubmitInput>()
        .register::<InputRequestIdentity>()
        .register::<ConfigurationSnapshot>()
        .register::<ConfigurationSaveResult>()
        .register::<ConfigurationError>()
        .register::<OpenConfiguration>()
        .register::<SaveConfiguration>()
        .register::<ConfigurationSessionRequest>()
        .register::<SaveObsPassword>()
        .register::<CredentialPresence>()
        .register::<HistoryQuery>()
        .register::<HistoryPage>()
        .register::<HistoryQueryError>()
        .register::<InspectHistory>()
        .register::<RunRecord>();
    std::fs::create_dir_all(output)?;
    let path = output.join("index.ts");
    Typescript::default()
        .header("// Generated from Rust declarations. Regenerate with cargo run -p snenkbot_desktop --example export_contracts -- frontend/src/lib/contracts.\n")
        .export_to(&path, &types, specta_serde::PhasesFormat)?;
    let source = std::fs::read_to_string(&path)?;
    let normalized = source
        .lines()
        .map(|line| format!("{}\n", line.trim_end()))
        .collect::<String>();
    std::fs::write(path, normalized)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_schema_contracts_match_rust_declarations() {
        let directory = tempfile::tempdir().unwrap();
        export_contracts(directory.path()).unwrap();
        let checked_in =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../frontend/src/lib/contracts");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("index.ts")).unwrap(),
            std::fs::read_to_string(checked_in.join("index.ts")).unwrap(),
            "regenerate frontend contracts after changing Rust metadata"
        );
    }
}
