//! Render the shell or editor controls without opening a desktop window.
//! Usage: cargo run --example render_ui -- /tmp/editor.png editor 1240 900
use std::error::Error;
use std::fs::File;
use std::io::BufWriter;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, PhysicalSize, Rgb8Pixel, VecModel};

slint::slint! {
    import { WorkflowEditor, StepIcon } from "../ui/editor.slint";
    import { Theme } from "../ui/theme.slint";
    import { AppButton, ValueField } from "../ui/controls.slint";
    import "../assets/fonts/InterTight.ttf";

    export component EditorPreview inherits Window {
        default-font-family: "Inter Tight";
        default-font-size: 14px;
        background: Theme.ink;
        WorkflowEditor {
            workflow-title: "A little welcome";
            saved-label: "Saved";
            can-run: true;
            triggers: [{id: "trigger", label: "When chat matches", detail: "Chat command · !hello", icon: StepIcon.message, value: [{text: "!hello", is-output: false}], suffix: ""}];
            steps: [
                {id: "scene", label: "Switch scene to", detail: "OBS · Step 1", icon: StepIcon.broadcast, value: [{text: "Just Chatting", is-output: false}], suffix: ""},
                {id: "message", label: "Send", detail: "Twitch · Step 2", icon: StepIcon.message, value: [
                    {text: "Welcome in,", is-output: false},
                    {text: "Viewer name", is-output: true, source-icon: @image-url("../assets/icons/zap.svg")},
                    {text: "— make yourself at home.", is-output: false}
                ], suffix: "to chat"},
                {id: "hotkey", label: "Play hotkey", detail: "VTube Studio · Step 3", icon: StepIcon.control, value: [{text: "Wave", is-output: false}], suffix: ""}
            ];
            selected-step: "message";
            Text { text: "Send as"; color: Theme.muted; font-size: 12px; }
            ValueField { text: "Bot Account · SnenkBot"; }
            Rectangle { height: 16px; }
            Text { text: "Message"; color: Theme.muted; font-size: 12px; }
            Rectangle {
                height: 112px;
                background: Theme.field;
                border-color: Theme.border;
                border-width: 1px;
                border-radius: 4px;
                VerticalLayout {
                    padding: 12px;
                    spacing: 12px;
                    HorizontalLayout {
                        spacing: 12px;
                        Text { text: "Welcome in,"; color: Theme.text; vertical-alignment: center; }
                        ValueField { text: "Viewer name"; output: true; source-icon: @image-url("../assets/icons/zap.svg"); }
                        Rectangle { horizontal-stretch: 1; }
                    }
                    Text { text: "— make yourself at home."; color: Theme.text; }
                    Rectangle { vertical-stretch: 1; }
                }
            }
            Rectangle { height: 8px; }
            AppButton { text: "Insert variable"; width: 156px; }
            Text { text: "Uses your bot account to post in chat."; color: Theme.muted; font-size: 12px; }
            Rectangle { vertical-stretch: 1; }
            HorizontalLayout {
                alignment: end;
                spacing: 8px;
                AppButton { text: "Cancel"; width: 88px; }
                AppButton { text: "Done"; primary: true; width: 88px; }
            }
        }
    }
}

struct RenderPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for RenderPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err(
            "usage: render_ui OUTPUT.png shell|library|library-empty|library-create|library-create-error|library-unavailable|editor|editor-unavailable|editor-edit|editor-optional|editor-repairable|editor-rename|editor-scene|editor-hotkey|editor-action-chooser|editor-action-form|editor-trigger-picker|editor-trigger-form WIDTH HEIGHT"
                .into(),
        );
    }
    let width: u32 = args[3].parse()?;
    let height: u32 = args[4].parse()?;
    if !(640..=4096).contains(&width) || !(480..=4096).contains(&height) {
        return Err("render dimensions must be between 640×480 and 4096×4096".into());
    }
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(RenderPlatform(adapter.clone())))?;
    let size = PhysicalSize::new(width, height);
    match args[2].as_str() {
        "editor-branches" | "editor-error" | "connections" => {
            use snenk_bot::engine::{
                CallBinding, Condition, FailurePolicy, Input, Step, StepKind, Workflow,
            };
            use snenk_bot::workflows::WorkflowDefinition;
            use std::collections::BTreeMap;
            let definition = WorkflowDefinition::manual(Workflow {
                id: "Stream details".into(),
                revision: 3,
                overlap: false,
                steps: vec![Step {
                    id: "changed".into(),
                    on_failure: FailurePolicy::Stop,
                    kind: StepKind::If {
                        condition: Condition::Exists(Input::Trigger {
                            name: "title".into(),
                            fallback: None,
                        }),
                        then_steps: vec![Step {
                            id: "game".into(),
                            on_failure: FailurePolicy::Stop,
                            kind: StepKind::Call {
                                workflow_id: "game-marker".into(),
                                binding: CallBinding::Explicit {
                                    inputs: BTreeMap::new(),
                                },
                            },
                        }],
                        else_steps: vec![Step {
                            id: "title".into(),
                            on_failure: FailurePolicy::Stop,
                            kind: StepKind::Call {
                                workflow_id: "title-marker".into(),
                                binding: CallBinding::Explicit {
                                    inputs: BTreeMap::new(),
                                },
                            },
                        }],
                    },
                }],
                outputs: BTreeMap::new(),
            });
            let preview = snenk_bot::ui::WorkflowPreview::from_definition_with_titles(
                &definition,
                &[],
                &BTreeMap::from([
                    ("game-marker".into(), "Game marker".into()),
                    ("title-marker".into(), "Title marker".into()),
                ]),
            );
            let window = snenk_bot::ui::create_window(&snenk_bot::paths::AppPaths {
                config: "/example/config".into(),
                data: "/example/data".into(),
                state: "/example/state".into(),
            })?;
            window.set_page(1);
            window.set_selected_automation("Stream details".into());
            window.set_editor_open(true);
            window.set_editor_title(preview.title.into());
            window.set_editor_revision(preview.revision.into());
            window.set_editor_triggers(ModelRc::new(VecModel::from(preview.triggers)));
            window.set_editor_steps(ModelRc::new(VecModel::from(preview.steps)));
            window.set_editor_workflow_enabled(true);
            window.set_editor_can_run(true);
            window.set_editor_can_edit_fields(true);
            window.set_editor_can_compose(true);
            window.set_editor_can_add_trigger(true);
            window.set_connection_count(2);
            window.set_connection_total(4);
            window.set_connection_state(3);
            window.set_connections(ModelRc::new(VecModel::from(vec![
                snenk_bot::ui::ConnectionRow {
                    title: "OBS Studio".into(),
                    state: 2,
                    detail: "Connected".into(),
                    ..Default::default()
                },
                snenk_bot::ui::ConnectionRow {
                    title: "Twitch broadcaster account".into(),
                    state: 3,
                    detail: "Reconnect required · Ad triggers\nReconnect your broadcaster account to allow ad events. Chat and other authorized features remain available.".into(),
                    integration: "twitch".into(),
                    connection: "broadcaster".into(),
                },
                snenk_bot::ui::ConnectionRow {
                    title: "Twitch bot account (optional)".into(),
                    state: 0,
                    detail: "Not configured".into(),
                    ..Default::default()
                },
                snenk_bot::ui::ConnectionRow {
                    title: "Twitch events".into(),
                    state: 2,
                    detail: "Connected".into(),
                    ..Default::default()
                },
                snenk_bot::ui::ConnectionRow {
                    title: "VTube Studio".into(),
                    state: 3,
                    detail: "Authorization required in VTube Studio".into(),
                    ..Default::default()
                },
            ])));
            if args[2] == "editor-error" {
                window
                    .set_error_message("Twitch could not connect. Retrying automatically.".into());
            }
            window.show()?;
            adapter.set_size(size);
            if args[2] == "connections" {
                i_slint_backend_testing::ElementHandle::find_by_accessible_label(
                    &window,
                    "Connected 2/4. Show connection details",
                )
                .next()
                .ok_or("connection summary is missing")?
                .invoke_accessible_default_action();
            }
            render(&adapter, &args[1], size)?;
        }
        "shell" => {
            let window = snenk_bot::ui::create_window(&snenk_bot::paths::AppPaths {
                config: "/example/config".into(),
                data: "/example/data".into(),
                state: "/example/state".into(),
            })?;
            window.show()?;
            adapter.set_size(size);
            render(&adapter, &args[1], size)?;
        }
        "settings-obs" | "settings-vtube" | "settings-twitch" | "settings-general" | "history"
        | "home" | "history-detail" => {
            let window = snenk_bot::ui::create_window(&snenk_bot::paths::AppPaths {
                config: "/example/config".into(),
                data: "/example/data".into(),
                state: "/example/state".into(),
            })?;
            let page = match args[2].as_str() {
                "settings-obs" => 4,
                "settings-vtube" => 6,
                "settings-twitch" => 5,
                "settings-general" => 3,
                "history" => 2,
                _ => 0,
            };
            window.set_page(page);
            window.set_obs_health("Connected".into());
            window.set_obs_enabled(true);
            window.set_obs_host("localhost".into());
            window.set_obs_password("example-password".into());
            window.set_vtube_health("Disabled".into());
            window.set_vtube_pending(false);
            window.set_twitch_broadcaster("Broadcaster".into());
            window.set_twitch_bot_account("Bot account".into());
            window.set_automations(ModelRc::new(VecModel::from(vec![
                snenk_bot::ui::AutomationRow {
                    enabled: true,
                    id: "welcome".into(),
                    title: "A little welcome".into(),
                    revision: 1,
                    has_steps: true,
                    trigger_summary: "!hello".into(),
                    step_count: 3,
                    icon_kind: 0,
                    error: "".into(),
                },
            ])));
            window.set_run_history(ModelRc::new(VecModel::from(vec![
                snenk_bot::ui::HistoryRow {
                    run_id: "example-run".into(),
                    workflow_id: "welcome".into(),
                    title: "A little welcome".into(),
                    outcome: "Succeeded".into(),
                    trigger: "Manual".into(),
                    started_at: "2026-10-01 09:30 UTC".into(),
                    duration: "2.4 s".into(),
                    workflow_revision: "1".into(),
                    ..Default::default()
                },
            ])));
            if args[2] == "history-detail" {
                use snenk_bot::ui::{HistoryRow, HistoryStep};
                window.set_page(2);
                window.set_selected_history_run("example-run".into());
                window.set_selected_history_record(HistoryRow {
                    run_id: "example-run".into(),
                    workflow_id: "welcome".into(),
                    title: "A little welcome".into(),
                    outcome: "Succeeded".into(),
                    trigger: "Chat message".into(),
                    started_at: "2026-10-01 09:30 UTC".into(),
                    duration: "2.4 s".into(),
                    workflow_revision: "3".into(),
                    steps: ModelRc::new(VecModel::from(vec![
                        HistoryStep {
                            sequence: "0".into(),
                            number: "1".into(),
                            title: "One or More".into(),
                            outcome: "Succeeded".into(),
                            duration: "2.0 s".into(),
                            step_id: "group".into(),
                            workflow_id: "welcome".into(),
                            workflow_revision: "3".into(),
                            ..Default::default()
                        },
                        HistoryStep {
                            sequence: "1".into(),
                            number: "2".into(),
                            depth: 1,
                            title: "Send chat message".into(),
                            outcome: "Failed".into(),
                            duration: "950 ms".into(),
                            failure: "The action timed out.".into(),
                            uncertain: true,
                            step_id: "chat".into(),
                            workflow_id: "welcome".into(),
                            workflow_revision: "3".into(),
                            ..Default::default()
                        },
                        HistoryStep {
                            sequence: "2".into(),
                            number: "3".into(),
                            depth: 1,
                            title: "Play hotkey".into(),
                            outcome: "Succeeded".into(),
                            duration: "100 ms".into(),
                            step_id: "wave".into(),
                            workflow_id: "welcome".into(),
                            workflow_revision: "3".into(),
                            ..Default::default()
                        },
                    ])),
                    ..Default::default()
                });
            }
            window.show()?;
            adapter.set_size(size);
            render(&adapter, &args[1], size)?;
        }
        "library"
        | "library-empty"
        | "library-create"
        | "library-create-error"
        | "library-unavailable" => {
            let window = snenk_bot::ui::create_window(&snenk_bot::paths::AppPaths {
                config: "/example/config".into(),
                data: "/example/data".into(),
                state: "/example/state".into(),
            })?;
            window.set_page(1);
            window.set_selected_automation("stable-id".into());
            let unavailable = args[2] == "library-unavailable";
            if unavailable {
                window.set_error_message(
                    "Workflow `stable-id` is unavailable: action module is missing".into(),
                );
            }
            let rows = vec![
                snenk_bot::ui::AutomationRow {
                    enabled: true,
                    id: "stable-id".into(),
                    title: "A lovely stream".into(),
                    revision: 1,
                    has_steps: true,
                    error: if unavailable {
                        "The saved action requires a module that is not available."
                    } else {
                        ""
                    }
                    .into(),
                    trigger_summary: "!hello".into(),
                    step_count: 3,
                    icon_kind: 0,
                },
                snenk_bot::ui::AutomationRow {
                    enabled: true,
                    id: "new-draft".into(),
                    title: "New draft".into(),
                    revision: 1,
                    has_steps: false,
                    error: "".into(),
                    trigger_summary: "Manual".into(),
                    step_count: 0,
                    icon_kind: 2,
                },
            ];
            window.set_automations(ModelRc::new(VecModel::from(
                if args[2] == "library-empty" {
                    Vec::new()
                } else {
                    rows
                },
            )));
            if args[2] == "library-create" || args[2] == "library-create-error" {
                window.set_create_visible(true);
                window.set_create_name("A new automation".into());
            }
            if args[2] == "library-create-error" {
                window.set_create_error(
                    "A name is required before this automation can be created".into(),
                );
            }
            window.show()?;
            adapter.set_size(size);
            render(&adapter, &args[1], size)?;
        }
        "editor" => {
            let window = EditorPreview::new()?;
            window.show()?;
            adapter.set_size(size);
            render(&adapter, &args[1], size)?;
        }
        "editor-unavailable" => {
            let window = snenk_bot::ui::create_window(&snenk_bot::paths::AppPaths {
                config: "/example/config".into(),
                data: "/example/data".into(),
                state: "/example/state".into(),
            })?;
            window.set_page(1);
            window.set_selected_automation("stable-id".into());
            window.set_editor_title("A lovely stream".into());
            window.set_editor_revision("Saved revision 2".into());
            window.set_editor_steps(ModelRc::new(VecModel::from(vec![
                snenk_bot::ui::WorkflowStep {
                    display_label: Default::default(),
                    condition: Default::default(),
                    branch_header: false,
                    branch_empty: false,
                    id: "step:future".into(),
                    label: "1 · future action".into(),
                    detail: "This action requires a module that is not available".into(),
                    icon: snenk_bot::ui::StepIcon::Control,
                    value: ModelRc::new(VecModel::from(Vec::new())),
                    suffix: "".into(),
                    control_kind: "".into(),
                    depth: 0,
                    group_end: false,
                    parent_id: "".into(),
                    draft_child: false,
                },
            ])));
            window.set_editor_open(true);
            window.set_error_message(
                "This workflow can be inspected, but an action module is unavailable".into(),
            );
            window.show()?;
            adapter.set_size(size);
            render(&adapter, &args[1], size)?;
        }
        "editor-control"
        | "editor-values"
        | "editor-compose"
        | "editor-resource"
        | "editor-edit"
        | "editor-optional"
        | "editor-repairable"
        | "editor-rename"
        | "editor-scene"
        | "editor-hotkey"
        | "editor-action-chooser"
        | "editor-action-form"
        | "editor-trigger-picker"
        | "editor-trigger-form" => {
            let optional = args[2] == "editor-optional";
            let repairable = args[2] == "editor-repairable";
            let renaming = args[2] == "editor-rename";
            let title = if optional {
                "Set stream details"
            } else if repairable {
                "Repair chat message"
            } else {
                "A little welcome"
            };
            let step = if optional {
                "Update channel"
            } else {
                "Send chat message"
            };
            let field_value = if optional {
                "Morning stream"
            } else if repairable {
                "false"
            } else {
                "Welcome to the stream!"
            };
            let window = snenk_bot::ui::create_window(&snenk_bot::paths::AppPaths {
                config: "/example/config".into(),
                data: "/example/data".into(),
                state: "/example/state".into(),
            })?;
            window.set_page(1);
            window.set_selected_automation("welcome".into());
            window.set_editor_title(title.into());
            window.set_editor_revision("Saved revision 3".into());
            window.set_editor_can_run(!repairable);
            window.set_editor_can_edit_fields(true);
            window.set_editor_can_compose(true);
            window.set_editor_can_add_trigger(true);
            window.set_editor_can_rename(!repairable);
            window.set_editor_selected_step("step:action".into());
            if args[2] == "editor-trigger-picker" {
                window.set_editor_trigger_mode(1);
                window.set_editor_trigger_choices(ModelRc::new(VecModel::from(vec![
                    snenk_bot::ui::TriggerChoice {
                        kind: "obs.recording_started".into(),
                        title: "OBS recording started".into(),
                        icon: snenk_bot::ui::StepIcon::Broadcast,
                    },
                    snenk_bot::ui::TriggerChoice {
                        kind: "obs.current_scene".into(),
                        title: "OBS current scene".into(),
                        icon: snenk_bot::ui::StepIcon::Broadcast,
                    },
                    snenk_bot::ui::TriggerChoice {
                        kind: "twitch.channel.updated".into(),
                        title: "Twitch channel updated".into(),
                        icon: snenk_bot::ui::StepIcon::Message,
                    },
                    snenk_bot::ui::TriggerChoice {
                        kind: "twitch.channel.details_changed".into(),
                        title: "Twitch title or game changed".into(),
                        icon: snenk_bot::ui::StepIcon::Message,
                    },
                ])));
            }
            if args[2] == "editor-trigger-form" {
                window.set_editor_trigger_mode(2);
                window.set_editor_trigger_kind("obs.current_scene".into());
                window.set_editor_trigger_title("OBS current scene".into());
                window.set_editor_trigger_scene("Starting Soon".into());
            }
            if args[2] == "editor-action-chooser" {
                window.set_editor_action_mode(1);
            }
            if args[2] == "editor-action-form" {
                window.set_editor_action_mode(2);
                window.set_editor_action_title("Send chat message".into());
                window.set_editor_action_fields(ModelRc::new(VecModel::from(vec![
                    snenk_bot::ui::ActionDraftField {
                        id: "message".into(),
                        label: "Message".into(),
                        description: "Text to send to chat".into(),
                        value: "Welcome to the stream!".into(),
                        kind: snenk_bot::ui::EditorFieldKind::Text,
                        required: true,
                        configured: true,
                        has_choices: false,
                    },
                ])));
            }
            window.set_editor_steps(ModelRc::new(VecModel::from(vec![
                snenk_bot::ui::WorkflowStep {
                    display_label: Default::default(),
                    condition: Default::default(),
                    branch_header: false,
                    branch_empty: false,
                    id: "step:action".into(),
                    label: format!("1 · {step}").into(),
                    detail: format!(
                        "{}: {field_value}",
                        if optional { "Title" } else { "Message" }
                    )
                    .into(),
                    icon: snenk_bot::ui::StepIcon::Message,
                    value: ModelRc::new(VecModel::from(vec![snenk_bot::ui::ValueSegment {
                        is_operator: false,
                        text: field_value.into(),
                        is_output: false,
                        source_icon: Default::default(),
                    }])),
                    suffix: "".into(),
                    control_kind: "".into(),
                    depth: 0,
                    group_end: false,
                    parent_id: "".into(),
                    draft_child: false,
                },
            ])));
            let mut fields = vec![snenk_bot::ui::EditorField {
                id: if optional { "title" } else { "message" }.into(),
                label: if optional { "Title" } else { "Message" }.into(),
                description: if optional {
                    "Title shown on the channel"
                } else {
                    "Text sent to the broadcaster's chat"
                }
                .into(),
                value: field_value.into(),
                kind: snenk_bot::ui::EditorFieldKind::Text,
                editable: true,
                is_output: false,
                configured: true,
                optional,
                has_choices: false,
            }];
            if optional {
                fields.push(snenk_bot::ui::EditorField {
                    id: "game_id".into(),
                    label: "Game ID".into(),
                    description: "Optional Twitch category".into(),
                    value: "Not set".into(),
                    kind: snenk_bot::ui::EditorFieldKind::Text,
                    editable: true,
                    is_output: false,
                    configured: false,
                    optional: true,
                    has_choices: false,
                });
            } else if !repairable && !renaming {
                window.set_editor_active_field("message".into());
                window.set_editor_draft_value(field_value.into());
            }
            window.set_editor_fields(ModelRc::new(VecModel::from(fields)));
            if renaming {
                window.set_editor_rename_active(true);
                window.set_editor_rename_draft("A little welcome".into());
            }
            if args[2] == "editor-action-chooser"
                || args[2] == "editor-action-form"
                || args[2] == "editor-trigger-picker"
                || args[2] == "editor-trigger-form"
            {
                window.set_editor_active_field("".into());
            }
            if matches!(
                args[2].as_str(),
                "editor-compose"
                    | "editor-resource"
                    | "editor-control"
                    | "editor-values"
                    | "editor-scene"
                    | "editor-hotkey"
                    | "editor-action-chooser"
                    | "editor-action-form"
                    | "editor-trigger-picker"
                    | "editor-trigger-form"
            ) {
                use snenk_bot::ui::{
                    ActionChoice, ResourceChoice, StepIcon, ValueSegment, WorkflowStep,
                };
                let literal = |text: &str| ValueSegment {
                    is_operator: false,
                    text: text.into(),
                    is_output: false,
                    source_icon: Default::default(),
                };
                window.set_editor_title("A little welcome".into());
                window.set_editor_active_field("".into());
                window.set_editor_selected_step("".into());
                window.set_editor_fields(ModelRc::new(VecModel::from(Vec::new())));
                window.set_editor_triggers(ModelRc::new(VecModel::from(vec![WorkflowStep {
                    display_label: Default::default(),
                    condition: Default::default(),
                    branch_header: false,
                    branch_empty: false,
                    id: "trigger:hello".into(),
                    label: "When chat matches".into(),
                    detail: "Twitch · Anyone".into(),
                    icon: StepIcon::Message,
                    value: ModelRc::new(VecModel::from(vec![literal("!hello")])),
                    suffix: "".into(),
                    control_kind: "".into(),
                    depth: 0,
                    group_end: false,
                    parent_id: "".into(),
                    draft_child: false,
                }])));
                window.set_editor_steps(ModelRc::new(VecModel::from(vec![
                    WorkflowStep {
                        display_label: Default::default(),
                        condition: Default::default(),
                        branch_header: false,
                        branch_empty: false,
                        id: "step:scene".into(),
                        label: "Switch scene to".into(),
                        detail: "".into(),
                        icon: StepIcon::Broadcast,
                        value: ModelRc::new(VecModel::from(vec![literal("Just Chatting")])),
                        suffix: "".into(),
                        control_kind: "".into(),
                        depth: 0,
                        group_end: false,
                        parent_id: "".into(),
                        draft_child: false,
                    },
                    WorkflowStep {
                        display_label: Default::default(),
                        condition: Default::default(),
                        branch_header: false,
                        branch_empty: false,
                        id: "step:message".into(),
                        label: "Send".into(),
                        detail: "".into(),
                        icon: StepIcon::Message,
                        value: ModelRc::new(VecModel::from(vec![
                            literal("Welcome in,"),
                            ValueSegment {
                                is_operator: false,
                                text: "Viewer name".into(),
                                is_output: true,
                                source_icon: Default::default(),
                            },
                            literal("— make yourself at home."),
                        ])),
                        suffix: "to chat".into(),
                        control_kind: "".into(),
                        depth: 0,
                        group_end: false,
                        parent_id: "".into(),
                        draft_child: false,
                    },
                    WorkflowStep {
                        display_label: Default::default(),
                        condition: Default::default(),
                        branch_header: false,
                        branch_empty: false,
                        id: "step:hotkey".into(),
                        label: "Play hotkey".into(),
                        detail: "".into(),
                        icon: StepIcon::Control,
                        value: ModelRc::new(VecModel::from(vec![literal("Wave")])),
                        suffix: "".into(),
                        control_kind: "".into(),
                        depth: 0,
                        group_end: false,
                        parent_id: "".into(),
                        draft_child: false,
                    },
                ])));
                window.set_editor_action_choices(ModelRc::new(VecModel::from(vec![
                    ActionChoice {
                        group: "OBS".into(),
                        capability: "obs.set_scene".into(),
                        version: 1,
                        title: "Switch scene".into(),
                        icon: StepIcon::Broadcast,
                    },
                    ActionChoice {
                        group: "Twitch".into(),
                        capability: "twitch.send_chat".into(),
                        version: 1,
                        title: "Send chat message".into(),
                        icon: StepIcon::Message,
                    },
                    ActionChoice {
                        group: "VTube Studio".into(),
                        capability: "vtube.play_hotkey".into(),
                        version: 1,
                        title: "Play hotkey".into(),
                        icon: StepIcon::Control,
                    },
                ])));
                window.set_editor_control_choices(ModelRc::new(VecModel::from(vec![
                    snenk_bot::ui::ControlChoice {
                        kind: "wait".into(),
                        title: "Wait".into(),
                    },
                    snenk_bot::ui::ControlChoice {
                        kind: "one_or_more".into(),
                        title: "One or More".into(),
                    },
                ])));
                if args[2] == "editor-scene" || args[2] == "editor-hotkey" {
                    let scene = args[2] == "editor-scene";
                    window.set_editor_selected_step(
                        if scene { "step:scene" } else { "step:hotkey" }.into(),
                    );
                    window.set_editor_fields(ModelRc::new(VecModel::from(vec![
                        snenk_bot::ui::EditorField {
                            id: if scene { "scene" } else { "hotkey" }.into(),
                            label: if scene { "Scene" } else { "Hotkey" }.into(),
                            description: "".into(),
                            value: if scene { "Just Chatting" } else { "Wave" }.into(),
                            kind: snenk_bot::ui::EditorFieldKind::Text,
                            editable: true,
                            is_output: false,
                            configured: true,
                            optional: false,
                            has_choices: true,
                        },
                    ])));
                }
                if args[2] == "editor-trigger-form" {
                    window.set_editor_selected_step("trigger:hello".into());
                }
                if args[2] == "editor-control" || args[2] == "editor-values" {
                    window.set_editor_steps(ModelRc::new(VecModel::from(vec![
                        WorkflowStep {
                            display_label: Default::default(),
                            condition: Default::default(),
                            branch_header: false,
                            branch_empty: false,
                            id: "step:group".into(),
                            label: "One or More".into(),
                            control_kind: "one_or_more".into(),
                            icon: StepIcon::Control,
                            ..Default::default()
                        },
                        WorkflowStep {
                            display_label: Default::default(),
                            condition: Default::default(),
                            branch_header: false,
                            branch_empty: false,
                            id: "step:chat".into(),
                            label: "Send chat message".into(),
                            suffix: "Twitch · Bot Account".into(),
                            depth: 1,
                            icon: StepIcon::Message,
                            ..Default::default()
                        },
                        WorkflowStep {
                            display_label: Default::default(),
                            condition: Default::default(),
                            branch_header: false,
                            branch_empty: false,
                            id: "step:wave".into(),
                            label: "Play hotkey".into(),
                            suffix: "VTube Studio · Wave".into(),
                            depth: 1,
                            icon: StepIcon::Control,
                            ..Default::default()
                        },
                        WorkflowStep {
                            display_label: Default::default(),
                            condition: Default::default(),
                            branch_header: false,
                            branch_empty: false,
                            id: "end:group".into(),
                            parent_id: "step:group".into(),
                            group_end: true,
                            control_kind: "body".into(),
                            ..Default::default()
                        },
                    ])));
                    window.set_editor_control_mode(1);
                    window.set_editor_control_kind("one_or_more".into());
                    window.set_editor_control_target("step:group".into());
                    window.set_editor_control_title("One or More".into());
                    if args[2] == "editor-values" {
                        use snenk_bot::ui::ValueChoice;
                        window.set_editor_value_visible(true);
                        window.set_editor_value_choices(ModelRc::new(VecModel::from(vec![
                            ValueChoice {
                                source: "trigger".into(),
                                source_id: "display_name".into(),
                                label: "Viewer name".into(),
                                detail: "Trigger · Chat message".into(),
                                fallback_kind: "text".into(),
                                ..Default::default()
                            },
                            ValueChoice {
                                source: "trigger".into(),
                                source_id: "message".into(),
                                label: "Message".into(),
                                detail: "Trigger · Chat message".into(),
                                optional: true,
                                fallback_kind: "text".into(),
                                ..Default::default()
                            },
                        ])));
                        window.set_editor_value_selected(1);
                        window.set_editor_value_fallback_kind("text".into());
                    }
                }
                if args[2] == "editor-resource" {
                    window.set_editor_selected_step("step:scene".into());
                    window.set_editor_choice_visible(true);
                    window.set_editor_choice_title("Choose Scene".into());
                    window.set_editor_choices(ModelRc::new(VecModel::from(vec![
                        ResourceChoice {
                            value: "Starting Soon".into(),
                            label: "Starting Soon".into(),
                            detail: "".into(),
                        },
                        ResourceChoice {
                            value: "Just Chatting".into(),
                            label: "Just Chatting".into(),
                            detail: "Current scene".into(),
                        },
                        ResourceChoice {
                            value: "Gameplay".into(),
                            label: "Gameplay".into(),
                            detail: "".into(),
                        },
                    ])));
                }
            }
            window.set_editor_open(true);
            if args[2] != "editor-compose" && args[2] != "editor-action-chooser" {
                window.set_editor_sidepanel_tab(1);
            }
            if repairable {
                window.set_error_message(
                    "This workflow cannot run because Message is invalid. Edit the field to repair it."
                        .into(),
                );
            }
            window.show()?;
            adapter.set_size(size);
            render(&adapter, &args[1], size)?;
        }
        _ => return Err("view must be shell, library, or editor".into()),
    }
    Ok(())
}

fn render(
    adapter: &MinimalSoftwareWindow,
    path: &str,
    size: PhysicalSize,
) -> Result<(), Box<dyn Error>> {
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![Rgb8Pixel::default(); size.width as usize * size.height as usize];
    adapter.request_redraw();
    adapter.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, size.width as usize);
    });
    let bytes: Vec<_> = pixels
        .iter()
        .flat_map(|pixel| [pixel.r, pixel.g, pixel.b])
        .collect();
    let mut encoder =
        png::Encoder::new(BufWriter::new(File::create(path)?), size.width, size.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&bytes)?;
    Ok(())
}
