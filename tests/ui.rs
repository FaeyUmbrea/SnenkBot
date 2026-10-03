use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, VecModel};
use snenk_bot::paths::AppPaths;
use snenk_bot::ui::{
    ActionChoice, ActionDraftField, AutomationRow, EditorFieldKind, PromptField, StepIcon,
    TriggerChoice, UiInputProvider, WorkflowStep, create_window,
};

#[test]
fn sidepanel_tabs_preserve_search_and_unfinished_action_fields() {
    i_slint_backend_testing::init_no_event_loop();
    let window = create_window(&AppPaths {
        config: "/test/config".into(),
        data: "/test/data".into(),
        state: "/test/state".into(),
    })
    .unwrap();
    window.set_page(1);
    window.set_editor_open(true);
    window.set_editor_can_compose(true);
    window.set_editor_action_mode(2);
    window.set_editor_action_query("scene".into());
    window.set_editor_action_category("obs".into());
    window.set_editor_action_fields(ModelRc::new(VecModel::from(vec![ActionDraftField {
        id: "message".into(),
        value: "Unfinished message".into(),
        ..Default::default()
    }])));
    for label in ["Actions tab", "Details tab", "Actions tab", "Details tab"] {
        ElementHandle::find_by_accessible_label(&window, label)
            .next()
            .unwrap()
            .invoke_accessible_default_action();
    }
    assert_eq!(window.get_editor_sidepanel_tab(), 1);
    assert_eq!(window.get_editor_action_mode(), 2);
    assert_eq!(window.get_editor_action_query(), "scene");
    assert_eq!(window.get_editor_action_category(), "obs");
    use slint::Model;
    assert_eq!(
        window.get_editor_action_fields().row_data(0).unwrap().value,
        "Unfinished message"
    );
}

#[test]
fn branded_connection_checkboxes_preserve_native_actions_and_busy_guards() {
    i_slint_backend_testing::init_no_event_loop();
    let window = create_window(&AppPaths {
        config: "/test/config/snenkbot".into(),
        data: "/test/data/snenkbot".into(),
        state: "/test/state/snenkbot".into(),
    })
    .unwrap();
    window.set_page(4);
    let obs = ElementHandle::find_by_accessible_label(&window, "Enable OBS")
        .next()
        .expect("OBS enable checkbox remains accessible");
    obs.invoke_accessible_default_action();
    assert!(window.get_obs_enabled());
    let tls = ElementHandle::find_by_accessible_label(&window, "Use TLS")
        .next()
        .expect("TLS checkbox remains accessible");
    tls.invoke_accessible_default_action();
    assert!(window.get_obs_tls());
    window.set_obs_save_pending(true);
    obs.invoke_accessible_default_action();
    tls.invoke_accessible_default_action();
    assert!(window.get_obs_enabled());
    assert!(window.get_obs_tls());

    window.set_page(6);
    window.set_vtube_pending(false);
    let vtube = ElementHandle::find_by_accessible_label(&window, "Enable VTube Studio")
        .next()
        .expect("VTube enable checkbox remains accessible");
    vtube.invoke_accessible_default_action();
    assert!(window.get_vtube_enabled());
    window.set_vtube_pending(true);
    vtube.invoke_accessible_default_action();
    assert!(window.get_vtube_enabled());
}

#[test]
fn app_window_navigation_paths_and_error_dismissal() {
    i_slint_backend_testing::init_no_event_loop();

    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();

    assert_eq!(window.get_page(), 0);

    let automations = ElementHandle::find_by_accessible_label(&window, "Automations")
        .next()
        .expect("Automations navigation item should be accessible");
    automations.invoke_accessible_default_action();
    assert_eq!(window.get_page(), 1);

    let general = ElementHandle::find_by_accessible_label(&window, "General")
        .next()
        .expect("General navigation item should be accessible");
    general.invoke_accessible_default_action();
    assert_eq!(window.get_page(), 3);

    assert_eq!(
        window.get_data_path().as_str(),
        paths.workflows_dir().to_string_lossy().as_ref()
    );
    assert_eq!(
        window.get_config_path().as_str(),
        paths.integrations_dir().to_string_lossy().as_ref()
    );
    assert_eq!(
        window.get_history_path().as_str(),
        paths.history_dir().to_string_lossy().as_ref()
    );

    for page in 0..=6 {
        window.set_page(page);
        window.set_error_message("test error".into());
        let dismiss = ElementHandle::find_by_accessible_label(&window, "Dismiss error")
            .next()
            .expect("error dismiss button should be accessible on every page");
        dismiss.invoke_accessible_default_action();
        assert_eq!(window.get_error_message().as_str(), "");
    }
}

#[test]
fn connection_details_and_workflow_activation_preserve_editor_context() {
    i_slint_backend_testing::init_no_event_loop();
    let window = create_window(&AppPaths {
        config: "/test/config".into(),
        data: "/test/data".into(),
        state: "/test/state".into(),
    })
    .unwrap();
    window.set_page(1);
    window.set_editor_open(true);
    window.set_selected_automation("workflow".into());
    window.set_editor_workflow_enabled(true);
    ElementHandle::find_by_accessible_label(&window, "Details tab")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    window.set_connection_count(2);
    window.set_connection_total(3);
    let enabled = Arc::new(Mutex::new(None));
    let received = Arc::clone(&enabled);
    window.on_set_editor_workflow_enabled(move |value| *received.lock().unwrap() = Some(value));
    ElementHandle::find_by_accessible_label(&window, "Disable workflow")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*enabled.lock().unwrap(), Some(false));
    ElementHandle::find_by_accessible_label(&window, "Connected 2/3. Show connection details")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    ElementHandle::find_by_accessible_label(&window, "Close connection details")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(window.get_page(), 1);
    assert_eq!(window.get_selected_automation(), "workflow");
}

#[test]
fn saved_automation_opens_from_the_library_and_runs_from_the_editor() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(1);
    window.set_automations(ModelRc::new(VecModel::from(vec![AutomationRow {
        enabled: true,
        id: "title-change".into(),
        title: "Change title".into(),
        revision: 2,
        has_steps: true,
        error: "".into(),
        trigger_summary: "Manual".into(),
        step_count: 2,
        icon_kind: 2,
    }])));
    let selected = ElementHandle::find_by_accessible_label(&window, "Open Change title")
        .next()
        .expect("saved automation should be accessible");
    let opened = Arc::new(Mutex::new(None));
    let received = Arc::clone(&opened);
    window.on_open_automation(move |id| *received.lock().unwrap() = Some(id.to_string()));
    selected.invoke_accessible_default_action();
    assert_eq!(window.get_selected_automation().as_str(), "title-change");
    assert_eq!(opened.lock().unwrap().as_deref(), Some("title-change"));

    let invoked = Arc::new(Mutex::new(None));
    let received = Arc::clone(&invoked);
    window.on_run_automation(move |id| *received.lock().unwrap() = Some(id.to_string()));
    window.set_editor_open(true);
    window.set_editor_can_run(true);
    let run = ElementHandle::find_by_accessible_label(&window, "Run automation")
        .next()
        .expect("editor should expose a play action");
    run.invoke_accessible_default_action();
    assert_eq!(invoked.lock().unwrap().as_deref(), Some("title-change"));
}

#[test]
fn opening_an_automation_keeps_a_direct_route_back_to_the_library() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(1);
    window.set_selected_automation("welcome".into());
    window.set_automations(ModelRc::new(VecModel::from(vec![AutomationRow {
        enabled: true,
        id: "welcome".into(),
        title: "A lovely stream".into(),
        revision: 1,
        has_steps: true,
        error: "".into(),
        trigger_summary: "!hello".into(),
        step_count: 3,
        icon_kind: 0,
    }])));
    let opened = Arc::new(Mutex::new(None));
    let received = Arc::clone(&opened);
    window.on_open_automation(move |id| *received.lock().unwrap() = Some(id.to_string()));
    ElementHandle::find_by_accessible_label(&window, "Open A lovely stream")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(opened.lock().unwrap().as_deref(), Some("welcome"));

    window.set_editor_title("A lovely stream".into());
    window.set_editor_triggers(ModelRc::new(VecModel::from(vec![WorkflowStep {
        display_label: Default::default(),
        condition: Default::default(),
        branch_header: false,
        branch_empty: false,
        id: "trigger:manual".into(),
        label: "Manual".into(),
        detail: "Enabled · Run from the app".into(),
        icon: StepIcon::Automation,
        value: ModelRc::new(VecModel::from(Vec::new())),
        suffix: "".into(),
        control_kind: "".into(),
        depth: 0,
        group_end: false,
        parent_id: "".into(),
        draft_child: false,
    }])));
    window.set_editor_open(true);
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Manual")
            .next()
            .is_some()
    );
    ElementHandle::find_by_accessible_label(&window, "Back to automations")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(!window.get_editor_open());
    assert_eq!(window.get_selected_automation().as_str(), "welcome");
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Open A lovely stream")
            .next()
            .is_some()
    );
}

#[test]
fn unavailable_saved_definition_can_be_inspected_but_not_run() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(1);
    window.set_selected_automation("future".into());
    window.set_automations(ModelRc::new(VecModel::from(vec![AutomationRow {
        enabled: true,
        id: "future".into(),
        title: "Future workflow".into(),
        revision: 2,
        has_steps: true,
        error: "Action is unavailable".into(),
        trigger_summary: "Manual".into(),
        step_count: 1,
        icon_kind: 2,
    }])));
    let opened = Arc::new(Mutex::new(None));
    let received = Arc::clone(&opened);
    window.on_open_automation(move |id| *received.lock().unwrap() = Some(id.to_string()));
    let run_count = Arc::new(Mutex::new(0));
    let received = Arc::clone(&run_count);
    window.on_run_automation(move |_| *received.lock().unwrap() += 1);
    ElementHandle::find_by_accessible_label(&window, "Open Future workflow")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(opened.lock().unwrap().as_deref(), Some("future"));
    assert_eq!(*run_count.lock().unwrap(), 0);

    window.set_editor_steps(ModelRc::new(VecModel::from(vec![WorkflowStep {
        display_label: Default::default(),
        condition: Default::default(),
        branch_header: false,
        branch_empty: false,
        id: "step:future".into(),
        label: "Unavailable action".into(),
        detail: "Action is unavailable".into(),
        icon: StepIcon::Control,
        value: ModelRc::new(VecModel::from(Vec::new())),
        suffix: "".into(),
        control_kind: "".into(),
        depth: 0,
        group_end: false,
        parent_id: "".into(),
        draft_child: false,
    }])));
    window.set_editor_open(true);
    ElementHandle::find_by_accessible_label(&window, "Run automation")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*run_count.lock().unwrap(), 0);
    window.set_editor_open(false);

    *opened.lock().unwrap() = None;
    window.set_automations(ModelRc::new(VecModel::from(vec![AutomationRow {
        enabled: true,
        id: "future".into(),
        title: "Future workflow".into(),
        revision: 0,
        has_steps: false,
        error: "Invalid JSON".into(),
        trigger_summary: "".into(),
        step_count: 0,
        icon_kind: 2,
    }])));
    ElementHandle::find_by_accessible_label(&window, "Open Future workflow")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(opened.lock().unwrap().is_none());
}

#[test]
fn library_creation_dialog_keeps_its_name_until_create_or_cancel() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(1);

    ElementHandle::find_by_accessible_label(&window, "Create automation")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(window.get_create_visible());
    window.set_create_name("My stream".into());

    let created = Arc::new(Mutex::new(None));
    let received = Arc::clone(&created);
    window.on_create_workflow(move |name| *received.lock().unwrap() = Some(name.to_string()));
    ElementHandle::find_by_accessible_label(&window, "Create")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(created.lock().unwrap().as_deref(), Some("My stream"));
    assert!(window.get_create_visible());

    window.on_cancel_create_workflow({
        let weak = window.as_weak();
        move || {
            if let Some(window) = weak.upgrade() {
                window.set_create_visible(false);
                window.set_create_name("".into());
            }
        }
    });
    ElementHandle::find_by_accessible_label(&window, "Cancel")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(!window.get_create_visible());
    assert!(window.get_create_name().is_empty());
}

#[test]
fn action_composer_keeps_the_editor_context_and_exposes_form_controls() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(1);
    window.set_editor_open(true);
    window.set_editor_can_compose(true);
    window.set_editor_title("Welcome".into());
    window.set_editor_action_choices(ModelRc::new(VecModel::from(vec![ActionChoice {
        group: "Twitch".into(),
        capability: "twitch.send_chat".into(),
        version: 1,
        title: "Send chat message".into(),
        icon: StepIcon::Message,
    }])));

    ElementHandle::find_by_accessible_label(&window, "Add action")
        .next()
        .expect("editor should expose composition")
        .invoke_accessible_default_action();
    assert_eq!(window.get_editor_action_mode(), 1);
    assert!(window.get_editor_open());
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Add Send chat message")
            .next()
            .is_some()
    );

    window.set_editor_action_mode(2);
    window.set_editor_sidepanel_tab(1);
    window.set_editor_action_title("Send chat message".into());
    window.set_editor_action_fields(ModelRc::new(VecModel::from(vec![ActionDraftField {
        id: "message".into(),
        label: "Message".into(),
        description: "Text to send".into(),
        value: "Hello".into(),
        kind: EditorFieldKind::Text,
        required: true,
        configured: true,
        has_choices: false,
    }])));
    let saved = Arc::new(Mutex::new(0));
    let received = Arc::clone(&saved);
    window.on_save_editor_action(move || *received.lock().unwrap() += 1);
    ElementHandle::find_by_accessible_label(&window, "Add action")
        .last()
        .expect("form should expose save action")
        .invoke_accessible_default_action();
    assert_eq!(*saved.lock().unwrap(), 1);
    assert!(window.get_editor_open());
}

#[test]
fn supported_trigger_picker_returns_to_the_same_editor() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(1);
    window.set_editor_open(true);
    window.set_editor_can_add_trigger(true);
    window.set_editor_trigger_choices(ModelRc::new(VecModel::from(vec![TriggerChoice {
        kind: "obs.recording_started".into(),
        title: "OBS recording started".into(),
        icon: StepIcon::Broadcast,
    }])));
    let weak = window.as_weak();
    window.on_add_editor_trigger(move || {
        if let Some(window) = weak.upgrade() {
            window.set_editor_trigger_mode(1);
            window.set_editor_sidepanel_tab(1);
        }
    });
    ElementHandle::find_by_accessible_label(&window, "Add trigger")
        .next()
        .expect("available editor should show Add trigger")
        .invoke_accessible_default_action();
    assert_eq!(window.get_editor_trigger_mode(), 1);
    assert!(window.get_editor_open());
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Add OBS recording started")
            .next()
            .is_some()
    );

    let weak = window.as_weak();
    window.on_cancel_editor_trigger(move || {
        if let Some(window) = weak.upgrade() {
            window.set_editor_trigger_mode(0);
        }
    });
    ElementHandle::find_by_accessible_label(&window, "Back to editor")
        .next()
        .expect("picker should provide contextual Back")
        .invoke_accessible_default_action();
    assert_eq!(window.get_editor_trigger_mode(), 0);
    assert!(window.get_editor_open());
}

#[test]
fn required_input_stays_open_until_a_value_is_entered() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    let _provider = UiInputProvider::new(&window);
    window.set_input_fields(ModelRc::new(VecModel::from(vec![PromptField {
        id: "title".into(),
        label: "Title".into(),
        value: "".into(),
        required: true,
    }])));
    window.set_input_visible(true);
    window.invoke_submit_input();
    assert!(window.get_input_visible());
    assert_eq!(window.get_input_error().as_str(), "Title is required");

    window.invoke_input_changed(0, "Ready to stream".into());
    assert_eq!(window.get_input_error().as_str(), "");
    window.invoke_submit_input();
    assert!(!window.get_input_visible());
}

#[test]
fn obs_connection_controls_are_available_in_broadcast_apps() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(4);
    ElementHandle::find_by_accessible_label(&window, "OBS host")
        .next()
        .unwrap()
        .set_accessible_value("192.168.178.44");
    ElementHandle::find_by_accessible_label(&window, "OBS port")
        .next()
        .unwrap()
        .set_accessible_value("4456");
    let saved = Arc::new(Mutex::new(false));
    let observed = Arc::clone(&saved);
    window.on_save_obs_settings(move || *observed.lock().unwrap() = true);
    ElementHandle::find_by_accessible_label(&window, "Save connection")
        .next()
        .expect("OBS save control should be accessible")
        .invoke_accessible_default_action();
    assert!(*saved.lock().unwrap());
    *saved.lock().unwrap() = false;
    let enable = ElementHandle::find_by_accessible_label(&window, "Enable OBS")
        .next()
        .expect("OBS enable setting should have an action label");
    enable.invoke_accessible_default_action();
    assert!(window.get_obs_enabled());
    assert!(
        !*saved.lock().unwrap(),
        "Draft changes must wait for Save connection"
    );
    window.set_obs_save_pending(true);
    enable.invoke_accessible_default_action();
    assert!(
        window.get_obs_enabled(),
        "Configuration must not change during a save"
    );
    assert_eq!(window.get_obs_host().as_str(), "192.168.178.44");
    assert_eq!(window.get_obs_port().as_str(), "4456");
}

#[test]
fn twitch_sign_in_keeps_link_and_code_available_for_copying() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config/snenkbot"),
        data: PathBuf::from("/test/data/snenkbot"),
        state: PathBuf::from("/test/state/snenkbot"),
    };
    let window = create_window(&paths).unwrap();
    window.set_page(5);
    let chosen = Arc::new(Mutex::new(None));
    let target = Arc::clone(&chosen);
    window.on_start_twitch_login(move |bot| *target.lock().unwrap() = Some(bot));
    ElementHandle::find_by_accessible_label(&window, "Connect bot")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*chosen.lock().unwrap(), Some(true));
    window.set_twitch_login_bot(true);
    window.set_twitch_url("https://www.twitch.tv/activate".into());
    window.set_twitch_code("ABCD-1234".into());
    window.set_twitch_login_stage(2);
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Open in browser")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Copy link")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Copy code")
            .next()
            .is_some()
    );
    window.set_twitch_review_login("streamerbot".into());
    window.set_twitch_review_id("456".into());
    window.set_twitch_login_stage(3);
    assert!(
        ElementHandle::find_by_accessible_label(&window, "Use this account")
            .next()
            .is_some()
    );
}

#[test]
fn integration_settings_return_to_the_open_automation_without_losing_selection() {
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: PathBuf::from("/test/config"),
        data: PathBuf::from("/test/data"),
        state: PathBuf::from("/test/state"),
    };
    let window = create_window(&paths).unwrap();
    window.set_editor_open(true);
    window.set_selected_automation("daily".into());
    window.set_editor_selected_step("action-2".into());
    for page in [2, 4, 5, 6] {
        window.set_page(page);
        ElementHandle::find_by_accessible_label(&window, "Back to automation")
            .next()
            .expect("Settings and history must offer a direct return")
            .invoke_accessible_default_action();
        assert_eq!(window.get_page(), 1);
        assert!(window.get_editor_open());
        assert_eq!(window.get_selected_automation(), "daily");
        assert_eq!(window.get_editor_selected_step(), "action-2");
    }
}

#[test]
fn connection_indicators_preserve_editor_context_and_use_live_status() {
    i_slint_backend_testing::init_no_event_loop();
    let window = create_window(&AppPaths {
        config: "/test/config".into(),
        data: "/test/data".into(),
        state: "/test/state".into(),
    })
    .unwrap();
    window.set_page(1);
    window.set_editor_open(true);
    window.set_selected_automation("working".into());
    window.set_editor_selected_step("step:message".into());
    window.set_connection_count(1);
    window.set_connection_total(2);
    ElementHandle::find_by_accessible_label(&window, "Connected 1/2. Show connection details")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(window.get_page(), 1);
    assert_eq!(window.get_selected_automation(), "working");
    assert_eq!(window.get_editor_selected_step(), "step:message");
}

#[test]
fn library_and_history_search_keep_matching_unavailable_records() {
    use slint::Model;
    i_slint_backend_testing::init_no_event_loop();
    let paths = AppPaths {
        config: "/test/config".into(),
        data: "/test/data".into(),
        state: "/test/state".into(),
    };
    let window = create_window(&paths).unwrap();
    let rows = ModelRc::new(VecModel::from(vec![
        AutomationRow {
            enabled: true,
            id: "welcome".into(),
            title: "Welcome".into(),
            trigger_summary: "!hello".into(),
            error: "Unavailable integration".into(),
            revision: 1,
            ..Default::default()
        },
        AutomationRow {
            enabled: true,
            id: "other".into(),
            title: "Other".into(),
            ..Default::default()
        },
    ]));
    let matches = window.invoke_filter_automations(rows, "  !HELLO  ".into());
    assert_eq!(matches.row_count(), 1);
    assert_eq!(matches.row_data(0).unwrap().id, "welcome");
    let records = ModelRc::new(VecModel::from(vec![
        snenk_bot::ui::HistoryRow {
            run_id: "failed".into(),
            title: "Welcome".into(),
            outcome: "Failed".into(),
            error: "OBS disconnected".into(),
            ..Default::default()
        },
        snenk_bot::ui::HistoryRow {
            run_id: "success".into(),
            title: "Welcome".into(),
            outcome: "Succeeded".into(),
            ..Default::default()
        },
    ]));
    let matches = window.invoke_filter_history(records, true, " disconnected ".into());
    assert_eq!(matches.row_count(), 1);
    assert_eq!(matches.row_data(0).unwrap().run_id, "failed");
}

#[test]
fn fresh_composer_exposes_actions_and_resource_picker_preserves_the_editor() {
    i_slint_backend_testing::init_no_event_loop();
    let window = create_window(&AppPaths {
        config: "/test/config".into(),
        data: "/test/data".into(),
        state: "/test/state".into(),
    })
    .unwrap();
    window.set_page(1);
    window.set_editor_open(true);
    window.set_editor_can_compose(true);
    window.set_editor_action_choices(ModelRc::new(VecModel::from(vec![ActionChoice {
        group: "OBS".into(),
        capability: "obs.set_scene".into(),
        version: 1,
        title: "Switch scene".into(),
        icon: StepIcon::Broadcast,
    }])));
    let chosen = Arc::new(Mutex::new(None));
    let received = Arc::clone(&chosen);
    window.on_choose_editor_action(move |id, version| {
        *received.lock().unwrap() = Some((id.to_string(), version));
    });
    ElementHandle::find_by_accessible_label(&window, "Add Switch scene")
        .next()
        .expect("fresh editor should expose actions without a separate navigation step")
        .invoke_accessible_default_action();
    assert_eq!(window.get_editor_action_mode(), 1);
    assert_eq!(
        chosen.lock().unwrap().as_ref(),
        Some(&("obs.set_scene".into(), 1))
    );

    window.set_editor_action_mode(0);
    window.set_editor_selected_step("step:scene".into());
    window.set_editor_choice_visible(true);
    window.set_editor_choice_title("Choose Scene".into());
    window.set_editor_choices(ModelRc::new(VecModel::from(vec![
        snenk_bot::ui::ResourceChoice {
            value: "stable-scene-id".into(),
            label: "Just Chatting".into(),
            detail: "Current scene".into(),
        },
    ])));
    let resource = Arc::new(Mutex::new(None));
    let received = Arc::clone(&resource);
    window.on_choose_editor_choice(move |value| {
        *received.lock().unwrap() = Some(value.to_string());
    });
    ElementHandle::find_by_accessible_label(&window, "Choose Just Chatting")
        .next()
        .expect("resource picker should expose the readable label")
        .invoke_accessible_default_action();
    assert_eq!(resource.lock().unwrap().as_deref(), Some("stable-scene-id"));
    let weak = window.as_weak();
    window.on_cancel_editor_choices(move || {
        if let Some(window) = weak.upgrade() {
            window.set_editor_choice_visible(false);
        }
    });
    ElementHandle::find_by_accessible_label(&window, "Back")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(!window.get_editor_choice_visible());
    assert!(window.get_editor_open());
    assert_eq!(window.get_editor_selected_step(), "step:scene");
}

#[test]
fn nested_action_insertion_and_control_drafts_keep_editor_context() {
    i_slint_backend_testing::init_no_event_loop();
    let window = create_window(&AppPaths {
        config: "/test/config".into(),
        data: "/test/data".into(),
        state: "/test/state".into(),
    })
    .unwrap();
    window.set_page(1);
    window.set_selected_automation("workflow".into());
    window.set_editor_open(true);
    window.set_editor_can_edit_fields(true);
    window.set_editor_can_compose(true);
    window.set_editor_can_run(true);
    window.set_editor_steps(ModelRc::new(VecModel::from(vec![
        WorkflowStep {
            display_label: Default::default(),
            condition: Default::default(),
            branch_header: false,
            branch_empty: false,
            id: "step:group".into(),
            label: "One or More".into(),
            control_kind: "one_or_more".into(),
            ..Default::default()
        },
        WorkflowStep {
            id: "branch:group:body".into(),
            display_label: "Try each".into(),
            parent_id: "step:group".into(),
            control_kind: "body".into(),
            branch_header: true,
            branch_empty: true,
            depth: 1,
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
    let insertion = Arc::new(Mutex::new(None));
    let received = Arc::clone(&insertion);
    window.on_add_editor_child(move |parent, branch| {
        *received.lock().unwrap() = Some((parent.to_string(), branch.to_string()))
    });
    ElementHandle::find_by_accessible_label(&window, "Add action to Try each")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(
        *insertion.lock().unwrap(),
        Some(("step:group".into(), "body".into()))
    );

    let runs = Arc::new(Mutex::new(0));
    let received = Arc::clone(&runs);
    window.on_run_automation(move |_| *received.lock().unwrap() += 1);
    window.set_editor_control_mode(1);
    window.set_editor_sidepanel_tab(1);
    window.set_editor_control_target("step:group".into());
    ElementHandle::find_by_accessible_label(&window, "Run automation")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*runs.lock().unwrap(), 0);
    *insertion.lock().unwrap() = None;
    let staged_branch = Arc::new(Mutex::new(None));
    let received = Arc::clone(&staged_branch);
    window.on_begin_editor_control_child(move |branch| {
        *received.lock().unwrap() = Some(branch.to_string())
    });
    ElementHandle::find_by_accessible_label(&window, "Add action to Try each")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(insertion.lock().unwrap().is_none());
    assert_eq!(staged_branch.lock().unwrap().as_deref(), Some("body"));

    let cancelled = Arc::new(Mutex::new(0));
    let received = Arc::clone(&cancelled);
    window.on_cancel_editor_control_values(move || *received.lock().unwrap() += 1);
    window.set_editor_value_visible(true);
    ElementHandle::find_by_accessible_label(&window, "Back to editor")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*cancelled.lock().unwrap(), 1);
    assert_eq!(window.get_selected_automation(), "workflow");
    assert!(window.get_editor_open());
}

#[test]
fn reconnect_targets_the_requested_role_and_returns_to_the_unfinished_workflow() {
    i_slint_backend_testing::init_no_event_loop();
    let window = create_window(&AppPaths {
        config: "/test/config".into(),
        data: "/test/data".into(),
        state: "/test/state".into(),
    })
    .unwrap();
    snenk_bot::ui::connect_reconfiguration(&window);
    window.set_page(1);
    window.set_editor_open(true);
    window.set_selected_automation("workflow".into());
    window.set_editor_action_query("scene".into());
    window.set_editor_action_fields(ModelRc::new(VecModel::from(vec![ActionDraftField {
        id: "message".into(),
        value: "unfinished".into(),
        ..Default::default()
    }])));
    window.set_connection_count(1);
    window.set_connection_total(2);
    window.set_connections(ModelRc::new(VecModel::from(vec![
        snenk_bot::ui::ConnectionRow {
            title: "Twitch broadcaster account".into(),
            detail: "Reconnect required · Ad triggers".into(),
            state: 3,
            integration: "twitch".into(),
            connection: "broadcaster".into(),
        },
    ])));
    let role = Arc::new(Mutex::new(None));
    let received = role.clone();
    window.on_start_twitch_login(move |is_bot| *received.lock().unwrap() = Some(is_bot));
    ElementHandle::find_by_accessible_label(&window, "Connected 1/2. Show connection details")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    ElementHandle::find_by_accessible_label(&window, "Reconnect")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*role.lock().unwrap(), Some(false));
    assert_eq!(window.get_page(), 5);
    ElementHandle::find_by_accessible_label(&window, "Back to Automations")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(window.get_page(), 1);
    assert_eq!(window.get_selected_automation(), "workflow");
    assert_eq!(window.get_editor_action_query(), "scene");
    use slint::Model;
    assert_eq!(
        window.get_editor_action_fields().row_data(0).unwrap().value,
        "unfinished"
    );
    assert_eq!(window.get_reconfigure_return_page(), -1);
    window.invoke_reconfigure_connection("twitch".into(), "bot".into());
    assert_eq!(*role.lock().unwrap(), Some(true));
}
