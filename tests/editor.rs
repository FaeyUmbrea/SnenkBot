use std::cell::RefCell;
use std::rc::Rc;

use i_slint_backend_testing::ElementHandle;

slint::slint! {
    import { WorkflowEditor, StepIcon, EditorFieldKind } from "../ui/editor.slint";
    export component EditorTest inherits Window {
        width: 1240px;
        height: 900px;
        callback selected(string);
        callback run;
        callback back;
        callback applied(string, string, string);
        callback removed(string, string);
        callback renamed(string);
        callback undo;
        in property <bool> ready: false;
        in-out property <string> active-field;
        in-out property <string> draft-value;
        in-out property <bool> rename-active;
        in-out property <string> rename-draft;
        WorkflowEditor {
            sidepanel-tab: 1;
            workflow-title: "Welcome";
            can-run: root.ready;
            can-edit-fields: true;
            can-rename: true;
            can-undo: true;
            selected-step: "step:send";
            active-field <=> root.active-field;
            draft-value <=> root.draft-value;
            rename-active <=> root.rename-active;
            rename-draft <=> root.rename-draft;
            steps: [{id: "message", label: "Send", detail: "Send a message", icon: StepIcon.message,
                value: [{text: "Hello", is-output: false}, {text: "Viewer name", is-output: true, source-icon: @image-url("../assets/icons/zap.svg")}], suffix: "to chat"}];
            inspector-fields: [
                {id: "message", label: "Message", description: "Chat text", value: "Hello", kind: EditorFieldKind.text, editable: true, is-output: false, configured: true, optional: false},
                {id: "game", label: "Game", description: "Optional game", value: "Not set", kind: EditorFieldKind.text, editable: true, is-output: false, configured: false, optional: true},
                {id: "title", label: "Title", description: "Optional title", value: "Current title", kind: EditorFieldKind.text, editable: true, is-output: false, configured: true, optional: true}
            ];
            select-step(id) => { root.selected(id); }
            apply-field(step-id, field-id, value) => { root.applied(step-id, field-id, value); }
            remove-field(step-id, field-id) => { root.removed(step-id, field-id); }
            apply-name(name) => { root.renamed(name); }
            undo => { root.undo(); }
            run => { root.run(); }
            back => { root.back(); }
        }
    }
}

#[test]
fn rename_keeps_a_local_draft_until_saved_or_cancelled() {
    i_slint_backend_testing::init_no_event_loop();
    let window = EditorTest::new().unwrap();
    let renamed = Rc::new(RefCell::new(None));
    let received = renamed.clone();
    window.on_renamed(move |name| *received.borrow_mut() = Some(name.to_string()));
    ElementHandle::find_by_accessible_label(&window, "Rename workflow")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(window.get_rename_active());
    assert_eq!(window.get_rename_draft().as_str(), "Welcome");
    ElementHandle::find_by_accessible_label(&window, "Edit")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(window.get_active_field().as_str(), "");
    window.set_rename_draft("A new name".into());
    ElementHandle::find_by_accessible_label(&window, "Save name")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(renamed.borrow().as_deref(), Some("A new name"));
    ElementHandle::find_by_accessible_label(&window, "Cancel rename")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert!(!window.get_rename_active());
    assert!(window.get_rename_draft().is_empty());
}

#[test]
fn optional_fields_can_be_added_and_removed_from_the_inspector() {
    i_slint_backend_testing::init_no_event_loop();
    let window = EditorTest::new().unwrap();
    let applied = Rc::new(RefCell::new(None));
    let received = applied.clone();
    window.on_applied(move |step, field, value| {
        *received.borrow_mut() = Some((step.to_string(), field.to_string(), value.to_string()));
    });
    let removed = Rc::new(RefCell::new(None));
    let received_remove = removed.clone();
    window.on_removed(move |step, field| {
        *received_remove.borrow_mut() = Some((step.to_string(), field.to_string()));
    });

    ElementHandle::find_by_accessible_label(&window, "Add")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(window.get_active_field().as_str(), "game");
    assert_eq!(window.get_draft_value().as_str(), "");
    ElementHandle::find_by_accessible_label(&window, "Remove Title")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*removed.borrow(), None);
    window.set_draft_value("Just Chatting".into());
    ElementHandle::find_by_accessible_label(&window, "Done")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(
        *applied.borrow(),
        Some(("step:send".into(), "game".into(), "Just Chatting".into()))
    );
    ElementHandle::find_by_accessible_label(&window, "Cancel")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    ElementHandle::find_by_accessible_label(&window, "Remove Title")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(
        *removed.borrow(),
        Some(("step:send".into(), "title".into()))
    );
}

#[test]
fn inspector_done_submits_the_selected_action_and_cancel_discards_draft() {
    i_slint_backend_testing::init_no_event_loop();
    let window = EditorTest::new().unwrap();
    let applied = Rc::new(RefCell::new(None));
    let received = applied.clone();
    window.on_applied(move |step, field, value| {
        *received.borrow_mut() = Some((step.to_string(), field.to_string(), value.to_string()));
    });
    let undo_count = Rc::new(RefCell::new(0));
    let received_undo = undo_count.clone();
    window.on_undo(move || *received_undo.borrow_mut() += 1);
    let undo = ElementHandle::find_by_accessible_label(&window, "Undo edit")
        .next()
        .unwrap();
    undo.invoke_accessible_default_action();
    assert_eq!(*undo_count.borrow(), 1);
    ElementHandle::find_by_accessible_label(&window, "Edit")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(window.get_active_field().as_str(), "message");
    undo.invoke_accessible_default_action();
    assert_eq!(*undo_count.borrow(), 1);
    window.set_draft_value("Edited".into());
    ElementHandle::find_by_accessible_label(&window, "Done")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(
        *applied.borrow(),
        Some(("step:send".into(), "message".into(), "Edited".into()))
    );
    ElementHandle::find_by_accessible_label(&window, "Cancel")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(window.get_active_field().as_str(), "");
    undo.invoke_accessible_default_action();
    assert_eq!(*undo_count.borrow(), 2);
}

#[test]
fn editor_exposes_selection_and_guards_run() {
    i_slint_backend_testing::init_no_event_loop();
    let window = EditorTest::new().unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let selected_events = events.clone();
    window.on_selected(move |id| selected_events.borrow_mut().push(id.to_string()));
    let run_events = events.clone();
    window.on_run(move || run_events.borrow_mut().push("run".into()));
    let back_events = events.clone();
    window.on_back(move || back_events.borrow_mut().push("back".into()));

    ElementHandle::find_by_accessible_label(&window, "Send")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    let run = ElementHandle::find_by_accessible_label(&window, "Run automation")
        .next()
        .unwrap();
    run.invoke_accessible_default_action();
    assert_eq!(&*events.borrow(), &["message"]);
    window.set_ready(true);
    run.invoke_accessible_default_action();
    ElementHandle::find_by_accessible_label(&window, "Back to automations")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(&*events.borrow(), &["message", "run", "back"]);
}
