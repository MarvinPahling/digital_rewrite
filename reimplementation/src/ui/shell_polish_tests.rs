use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP: AtomicU64 = AtomicU64::new(0);
fn test_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "digital-shell-polish-{}-{}",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn app_for(dir: &std::path::Path) -> DigitalApp {
    DigitalApp::with_file_io(Some(Box::new(LocalFileIo::new(dir))), None)
}
fn frame(ctx: &egui::Context, app: &mut DigitalApp) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 650.))),
            focused: true,
            ..Default::default()
        },
        |ctx| app.show(ctx),
    )
}
fn title(ctx: &egui::Context, app: &mut DigitalApp) -> String {
    let previous = app.last_window_title.clone();
    let output = frame(ctx, app);
    let command = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .unwrap()
        .commands
        .iter()
        .find_map(|command| match command {
            egui::ViewportCommand::Title(value) => Some(value.clone()),
            _ => None,
        });
    let current = app.last_window_title.clone().unwrap();
    if previous.as_deref() == Some(&current) {
        assert!(
            command.is_none(),
            "unchanged titles must not issue viewport commands"
        );
    } else {
        assert_eq!(command.as_deref(), Some(current.as_str()));
    }
    current
}
#[derive(Default)]
struct Storage {
    values: std::collections::HashMap<String, String>,
    flushes: usize,
}
impl eframe::Storage for Storage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.values.insert(key.into(), value);
    }
    fn flush(&mut self) {
        self.flushes += 1;
    }
}

#[test]
fn viewport_title_reflects_session_path_dirty_baseline_undo_new_open_and_failure() {
    let ctx = egui::Context::default();
    let dir = test_dir();
    let mut app = app_for(&dir);
    assert_eq!(title(&ctx, &mut app), "Untitled — Digital Rust");
    let id = app
        .session
        .place(ComponentKind::Output, Position { x: 100., y: 100. })
        .unwrap();
    let path = dir.join("title.dig");
    app.save_as_path(path.clone(), false, false);
    assert_eq!(title(&ctx, &mut app), "title.dig — Digital Rust");
    app.session.set_label(id, "changed").unwrap();
    assert_eq!(title(&ctx, &mut app), "title.dig* — Digital Rust");
    app.session.undo().unwrap();
    assert_eq!(title(&ctx, &mut app), "title.dig — Digital Rust");
    app.session.redo().unwrap();
    assert_eq!(title(&ctx, &mut app), "title.dig* — Digital Rust");
    app.save_current(false);
    assert_eq!(title(&ctx, &mut app), "title.dig — Digital Rust");
    app.begin_pending(PendingAction::New);
    assert_eq!(title(&ctx, &mut app), "Untitled — Digital Rust");
    app.begin_pending(PendingAction::Open(path));
    assert_eq!(title(&ctx, &mut app), "title.dig — Digital Rust");
    let before = app.session.document().clone();
    let old_path = app.session.path().unwrap().to_path_buf();
    app.begin_pending(PendingAction::Open(dir.join("missing.dig")));
    assert_eq!(app.session.document(), &before);
    assert_eq!(app.session.path(), Some(old_path.as_path()));
    assert_eq!(title(&ctx, &mut app), "title.dig — Digital Rust");
    assert!(app.pending_action.is_none());
    assert_eq!(app.recent_remove, None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preferences_are_versioned_bounded_lossless_and_outside_document_history() {
    let mut prefs = UiPreferences::default();
    let path = std::path::PathBuf::from("/tmp/a/../odd circuit.dig");
    prefs.promote(path.clone()).unwrap();
    prefs.promote("/tmp/other.dig".into()).unwrap();
    prefs.promote(path.clone()).unwrap();
    assert_eq!(prefs.recent[0], path);
    assert_eq!(prefs.recent.len(), 2);
    let encoded = prefs.encode().unwrap();
    assert_eq!(UiPreferences::decode(&encoded).unwrap(), prefs);
    for index in 0..15 {
        prefs.promote(format!("/tmp/{index}.dig").into()).unwrap();
    }
    assert_eq!(prefs.recent.len(), 10);
    assert!(!prefs.recent.contains(&path));
    assert_eq!(
        UiPreferences::decode("unknown"),
        Err(PreferencesError::Version)
    );
    assert_eq!(
        UiPreferences::decode("digital-rust-ui-preferences\t7\n"),
        Err(PreferencesError::Version)
    );
    assert_eq!(
        UiPreferences::decode("digital-rust-ui-preferences\t1\ninvalid\n"),
        Err(PreferencesError::Malformed)
    );
    assert_eq!(
        UiPreferences::decode(&"x".repeat(16 * 1024 + 1)),
        Err(PreferencesError::Limit)
    );
    assert!(prefs.encode().is_ok());
    let mut app = DigitalApp::default();
    let component = app
        .session
        .place(ComponentKind::Input, Position { x: 80., y: 40. })
        .unwrap();
    app.preferences = prefs.clone();
    let document = app.session.document().clone();
    let mut storage = Storage::default();
    <DigitalApp as eframe::App>::save(&mut app, &mut storage);
    assert_eq!(storage.flushes, 1);
    let restarted = DigitalApp::default().with_storage_record(eframe::Storage::get_string(
        &storage,
        "digital-rust.ui-preferences",
    ));
    assert_eq!(restarted.preferences, prefs);
    assert_eq!(restarted.session.document(), &Document::default());
    assert_eq!(app.session.document(), &document);
    assert!(
        app.session
            .document()
            .components
            .iter()
            .any(|c| c.id == component)
    );
    let malformed =
        DigitalApp::default().with_storage_record(Some("digital-rust-ui-preferences\t99\n".into()));
    assert!(malformed.preferences.recent.is_empty());
    assert!(malformed.preferences_error.is_some());
    let huge = DigitalApp::default().with_storage_record(Some("x".repeat(16 * 1024 + 1)));
    assert!(huge.preferences.recent.is_empty());
    assert!(huge.preferences_error.is_some());
}

#[test]
fn recent_open_runs_dirty_continuations_and_mru_promotes_only_published_session_paths() {
    let ctx = egui::Context::default();
    let dir = test_dir();
    let mut app = app_for(&dir);
    let saved = dir.join("recent.dig");
    let id = app
        .session
        .place(ComponentKind::Input, Position { x: 80., y: 80. })
        .unwrap();
    app.save_as_path(saved.clone(), false, false);
    assert_eq!(app.preferences.recent.first(), Some(&saved));
    app.begin_pending(PendingAction::New);
    app.session
        .place(ComponentKind::Output, Position { x: 160., y: 80. })
        .unwrap();
    let before = app.session.document().clone();
    app.begin_pending(PendingAction::Open(saved.clone()));
    assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
    assert_eq!(app.session.document(), &before);
    app.file_modal = None;
    app.pending_action = None;
    app.session.undo().unwrap();
    app.begin_pending(PendingAction::Open(saved.clone()));
    assert_eq!(app.session.document().components[0].id, id);
    assert_eq!(app.preferences.recent.first(), Some(&saved));
    app.session.set_label(id, "dirty").unwrap();
    let before = app.session.document().clone();
    app.begin_pending(PendingAction::Open(dir.join("missing.dig")));
    assert!(matches!(app.file_modal, Some(FileModal::Unsaved)));
    app.file_modal = None;
    app.pending_action = None;
    assert_eq!(app.session.document(), &before);
    app.session.undo().unwrap();
    let clean_before = app.session.document().clone();
    app.begin_pending(PendingAction::Open(dir.join("missing.dig")));
    assert!(
        app.error
            .as_deref()
            .is_some_and(|e| e.contains("No such file") || e.contains("os error 2"))
    );
    assert!(app.recent_remove.is_none());
    assert_eq!(app.session.document(), &clean_before);
    app.preferences.remove(&saved);
    assert!(!app.preferences.recent.contains(&saved));
    let _ = frame(&ctx, &mut app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn action_matrix_has_precise_reasons_and_refuses_side_effects_under_locks() {
    let mut app = DigitalApp::default();
    let nothing = app.session.document().clone();
    assert!(!app.action_availability(ShellAction::Copy).enabled);
    assert_eq!(
        app.action_availability(ShellAction::Undo).reason,
        Some("Nothing to undo")
    );
    let id = app
        .session
        .place(ComponentKind::Input, Position { x: 100., y: 100. })
        .unwrap();
    let wire = app
        .session
        .add_geometric_segments(&[(
            digital_rust::geometric_wiring::Point::new(0, 0),
            digital_rust::geometric_wiring::Point::new(20, 0),
        )])
        .unwrap();
    assert!(wire);
    let wire_id = app.session.document().wires.geometric().unwrap()[0].id;
    app.selection
        .wires
        .insert(wire_id, crate::selection::EndpointMask::A);
    let partial = app.action_availability(ShellAction::Cut);
    assert!(!partial.enabled);
    assert!(partial.reason.unwrap().contains("partial"));
    app.selection.clear();
    app.selection.select_component(id, false, false);
    assert!(app.action_availability(ShellAction::Copy).enabled);
    assert!(app.action_availability(ShellAction::Delete).enabled);
    app.session.start_simulation().unwrap();
    let before = app.session.document().clone();
    let sim = app.action_availability(ShellAction::Delete);
    assert!(!sim.enabled);
    assert!(sim.reason.unwrap().contains("Stop"));
    app.run_shell_action(ShellAction::Delete);
    assert_eq!(app.session.document(), &before);
    app.session.stop_simulation().unwrap();
    app.file_modal = Some(FileModal::OpenPath);
    assert!(
        app.action_availability(ShellAction::Undo)
            .reason
            .unwrap()
            .contains("dialog")
    );
    app.file_modal = None;
    app.drag = Some((id, Position { x: 100., y: 100. }, Pos2::ZERO));
    assert!(
        app.action_availability(ShellAction::Copy)
            .reason
            .unwrap()
            .contains("gesture")
    );
    app.drag = None;
    assert_ne!(app.session.document(), &nothing);
}

#[test]
fn toolbar_icon_assets_decode_to_theme_mask_at_both_resolutions_and_cache() {
    let ids = [
        "file-new",
        "file-open",
        "file-save",
        "file-save-as",
        "edit-undo",
        "edit-redo",
        "edit-copy",
        "edit-delete",
        "view-fit-window",
    ];
    for id in ids {
        for px in [24, 48] {
            let image =
                decode_tinted_icon_png(crate::assets::icon_png(id, px).unwrap(), px, [19, 37, 61])
                    .unwrap();
            assert_eq!(image.size, [px as usize, px as usize]);
            assert!(image.pixels.iter().any(|p| p.a() > 0));
            let opaque = image
                .pixels
                .iter()
                .filter(|p| p.a() > 220)
                .collect::<Vec<_>>();
            assert!(!opaque.is_empty(), "{id} @{px} has no solid icon pixels");
            assert!(
                opaque.iter().all(|p| {
                    let [r, g, b, _] = p.to_srgba_unmultiplied();
                    [
                        (r as i16 - 19).abs(),
                        (g as i16 - 37).abs(),
                        (b as i16 - 61).abs(),
                    ]
                    .into_iter()
                    .all(|delta| delta <= 2)
                }),
                "{id} @{px} does not use the theme tint"
            );
        }
    }
    let ctx = egui::Context::default();
    let mut app = DigitalApp::default();
    frame(&ctx, &mut app);
    let cached = app.icon_textures.len();
    assert!(cached >= ids.len());
    frame(&ctx, &mut app);
    assert_eq!(app.icon_textures.len(), cached);
    assert!(app.icon_errors.is_empty());
}

#[test]
fn missing_recent_path_reports_not_found_and_can_be_removed_without_replacing_document() {
    let dir = test_dir();
    let mut app = app_for(&dir);
    let target = dir.join("gone.dig");
    app.preferences.promote(target.clone()).unwrap();
    let id = app
        .session
        .place(ComponentKind::Input, Position { x: 60., y: 60. })
        .unwrap();
    let doc = app.session.document().clone();
    app.save_as_path(dir.join("baseline.dig"), false, false);
    app.preferences.promote(target.clone()).unwrap();
    app.begin_pending(PendingAction::Open(target.clone()));
    assert!(
        app.error
            .as_deref()
            .is_some_and(|e| e.contains("No such file") || e.contains("os error 2"))
    );
    assert_eq!(app.recent_remove, Some(target.clone()));
    assert_eq!(app.session.document(), &doc);
    assert!(app.session.document().components.iter().any(|c| c.id == id));
    app.preferences.remove(&target);
    app.recent_remove = None;
    assert!(!app.preferences.recent.contains(&target));
    assert!(
        app.preferences
            .recent
            .iter()
            .all(|recent| recent.is_absolute())
    );
    std::fs::remove_dir_all(dir).unwrap();
}
