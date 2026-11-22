//! Top bar and status bar of the main window.
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui;

use super::{App, Phase, SCAN_REPAINT};
use crate::format::{format_bytes, format_count};
use crate::model::Metric;
use crate::scan::{Progress, ScanMethod, ScanResult, ScanTarget};
use crate::win::VolumeInfo;

const CANCELLING: &str = "Cancelling…";
const NOT_GRANTED: &str = "Administrator rights were not granted.";
/// The accessible name of the drive list.
const DRIVE_LIST_NAME: &str = "Drive or folder to scan";

/// What the user asked for in the top bar; applied by [`App`] after drawing.
pub(super) enum Action {
    /// The drive list was just opened: read it again.
    DriveListOpened,
    SelectVolume(char),
    PickFolder,
    Scan,
    Cancel,
    MetricChanged,
    Relaunch(char),
    SlowScan(ScanTarget),
}

/// Seconds with one decimal, e.g. `4.2`.
pub(super) fn format_seconds(d: Duration) -> String {
    let tenths = (d.as_millis() + 50) / 100;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// `Scanning` with one to three dots, one more every [`SCAN_REPAINT`] of `elapsed`.
pub(super) fn scanning_text(elapsed: Duration) -> String {
    let step = elapsed.as_millis() / SCAN_REPAINT.as_millis();
    let dots = (step % 3) as usize + 1;
    format!("Scanning{}", ".".repeat(dots))
}

/// The `Scanning` text, always as wide as its form with three dots, so that what
/// follows it in the row does not move when the dots change.
fn scanning_label(ui: &mut egui::Ui, elapsed: Duration) {
    let widest = egui::WidgetText::from(scanning_text(2 * SCAN_REPAINT))
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body)
        .size();
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(widest, layout, |ui| {
        ui.set_min_width(widest.x);
        ui.label(scanning_text(elapsed));
    });
}

/// What a running scan shows: `Cancelling…` once Cancel was pressed, else the dots.
pub(super) fn scan_indicator(progress: &Progress, elapsed: Duration) -> String {
    if progress.cancel.load(Ordering::Relaxed) {
        CANCELLING.to_string()
    } else {
        scanning_text(elapsed)
    }
}

/// A drive as shown in the drive list, e.g. `C:  NTFS  3.7 TB`.
fn volume_label(v: &VolumeInfo) -> String {
    format!("{}:  {}  {}", v.letter, v.fs_name, format_bytes(v.total))
}

fn target_label(app: &App) -> String {
    match &app.target {
        None => String::new(),
        Some(ScanTarget::Folder(path)) => path.display().to_string(),
        Some(ScanTarget::Volume(letter)) => app
            .volumes
            .iter()
            .find(|v| v.letter == *letter)
            .map_or_else(|| format!("{letter}:"), volume_label),
    }
}

/// The status bar line for a finished scan.
pub fn status_text(result: &ScanResult, volume: Option<&VolumeInfo>) -> String {
    let mut parts = Vec::new();
    if let Some(v) = volume {
        parts.push(format!("Free: {} of {}", format_bytes(v.free), format_bytes(v.total)));
    }
    let files = result.tree.file_count(result.tree.root());
    parts.push(format!("{} files", format_count(u64::from(files))));
    let seconds = format_seconds(result.duration);
    parts.push(match result.method {
        ScanMethod::Mft => format!("MFT scan, {seconds} s"),
        ScanMethod::Walk => format!("Folder scan, allocated approximate, {seconds} s"),
    });
    for (n, what) in [
        (result.skipped, "skipped"),
        (result.unreadable, "unreadable"),
        (result.online_only, "online-only"),
    ] {
        if n > 0 {
            parts.push(format!("{} {what}", format_count(n)));
        }
    }
    parts.join(" | ")
}

/// Drive list, folder picker, scan button, metric switch, treemap toggle,
/// progress and the elevation / error notice. `now` is the clock for the elapsed time.
pub(super) fn top_bar(ui: &mut egui::Ui, app: &mut App, now: Instant) -> Option<Action> {
    let mut action = None;
    let (scanning, cancelling) = match &app.phase {
        Phase::Scanning { progress, .. } => (true, progress.cancel.load(Ordering::Relaxed)),
        _ => (false, false),
    };
    ui.horizontal(|ui| {
        let list_open = ui
            .add_enabled_ui(!scanning, |ui| {
                let list = egui::ComboBox::from_id_salt("target")
                    .width(220.0)
                    .selected_text(target_label(app))
                    .show_ui(ui, |ui| {
                        for v in &app.volumes {
                            let selected = matches!(app.target, Some(ScanTarget::Volume(l)) if l == v.letter);
                            if ui.selectable_label(selected, volume_label(v)).clicked() {
                                action = Some(Action::SelectVolume(v.letter));
                            }
                        }
                    });
                // Nothing visible names the list; its value is the chosen drive or folder.
                let _ = list.response.accessible_name(DRIVE_LIST_NAME);
                list.inner.is_some()
            })
            .inner;
        if list_open && !app.drive_list_open {
            action = Some(Action::DriveListOpened);
        }
        app.drive_list_open = list_open;
        if ui.add_enabled(!scanning && app.folder_rx.is_none(), egui::Button::new("Folder…")).clicked() {
            action = Some(Action::PickFolder);
        }
        if scanning {
            // Disabled after Cancel until the scan thread has ended.
            if ui.add_enabled(!cancelling, egui::Button::new("Cancel")).clicked() {
                action = Some(Action::Cancel);
            }
        } else if ui.add_enabled(app.target.is_some(), egui::Button::new("Scan")).clicked() {
            action = Some(Action::Scan);
        }
        ui.separator();
        // One group named `Show` for screen readers; the visible `Show:` says the same.
        let group = egui::UiBuilder::new().accessibility_role(egui::Role::RadioGroup).accessibility_label("Show");
        let metric_changed = ui
            .scope_builder(group, |ui| {
                let label = ui.label("Show:");
                ui.ctx().accesskit_node_builder(label.id, |node| node.set_hidden());
                let allocated = ui.radio_value(&mut app.metric, Metric::Allocated, "Allocated");
                let size = ui.radio_value(&mut app.metric, Metric::Size, "Size");
                allocated.changed() || size.changed()
            })
            .inner;
        if metric_changed {
            action = Some(Action::MetricChanged);
        }
        ui.separator();
        ui.toggle_value(&mut app.show_treemap, "Treemap");
        if let Phase::Scanning { progress, started, method, .. } = &app.phase {
            ui.separator();
            let elapsed = now.saturating_duration_since(*started);
            let done = progress.done.load(Ordering::Relaxed);
            if cancelling {
                ui.label(CANCELLING);
            } else {
                match method {
                    ScanMethod::Mft => {
                        let total = progress.total.load(Ordering::Relaxed);
                        let fraction = if total == 0 { 0.0 } else { (done as f64 / total as f64).min(1.0) as f32 };
                        ui.add(egui::ProgressBar::new(fraction).desired_width(200.0).show_percentage());
                    }
                    ScanMethod::Walk => {
                        scanning_label(ui, elapsed);
                        ui.label(format!("{} entries found", format_count(done)));
                    }
                }
            }
            ui.weak(format!("{} s", format_seconds(elapsed)));
        }
    });
    if let Some(hint) = app.hint {
        ui.horizontal(|ui| {
            ui.label("The fast scan needs administrator rights.");
            if ui.button("Restart as administrator").clicked() {
                action = Some(Action::Relaunch(hint.letter));
            }
            if ui.button("Scan slowly").clicked() {
                action = Some(Action::SlowScan(ScanTarget::Volume(hint.letter)));
            }
        });
        if hint.relaunch_failed {
            ui.colored_label(ui.visuals().error_fg_color, NOT_GRANTED);
        }
    }
    if let Phase::Failed { message, offer_walk } = &app.phase {
        ui.horizontal(|ui| {
            ui.colored_label(ui.visuals().error_fg_color, message);
            if let Some(target) = offer_walk
                && ui.button("Scan slowly").clicked()
            {
                action = Some(Action::SlowScan(target.clone()));
            }
        });
    }
    action
}

/// One line at the bottom; only a finished scan has something to say.
pub(super) fn status_bar(ui: &mut egui::Ui, phase: &Phase) {
    let text = match phase {
        Phase::Done { result, volume, .. } => status_text(result, volume.as_ref()),
        _ => String::new(),
    };
    ui.label(text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{F_DIR, NO_PARENT, TreeBuilder};

    fn result_with_files(files: u32, method: ScanMethod, millis: u64) -> ScanResult {
        let mut b = TreeBuilder::with_capacity(files as usize + 1, files as usize + 2);
        let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR);
        for _ in 0..files {
            b.push("f", root, 1, 4096, 0, 0);
        }
        let (tree, _) = b.finish(root);
        ScanResult {
            tree,
            method,
            duration: Duration::from_millis(millis),
            skipped: 0,
            unreadable: 0,
            online_only: 0,
        }
    }

    #[test]
    fn status_for_mft_scan_of_a_volume() {
        let mut result = result_with_files(1_683_667, ScanMethod::Mft, 4200);
        result.skipped = 3;
        let volume = VolumeInfo {
            letter: 'C',
            fs_name: "NTFS".into(),
            total: 4_090_260_848_640,
            free: 1_407_374_883_553,
        };
        let s = status_text(&result, Some(&volume));
        assert_eq!(s, "Free: 1.3 TB of 3.7 TB | 1,683,667 files | MFT scan, 4.2 s | 3 skipped");
    }

    #[test]
    fn status_for_walk_of_a_folder() {
        let mut result = result_with_files(12, ScanMethod::Walk, 800);
        result.unreadable = 2;
        result.online_only = 1;
        let s = status_text(&result, None);
        assert_eq!(s, "12 files | Folder scan, allocated approximate, 0.8 s | 2 unreadable | 1 online-only");
    }

    #[test]
    fn scanning_dots_follow_the_clock() {
        let text = |ms| scanning_text(Duration::from_millis(ms));
        assert_eq!(text(0), "Scanning.");
        assert_eq!(text(249), "Scanning.");
        assert_eq!(text(250), "Scanning..");
        assert_eq!(text(500), "Scanning...");
        assert_eq!(text(750), "Scanning.");
        // u64::MAX / 250 = 73,786,976,294,958,206, which leaves 2 when divided by 3.
        assert_eq!(text(u64::MAX), "Scanning...");
    }

    #[test]
    fn fixed_texts() {
        assert_eq!(CANCELLING, "Cancelling\u{2026}");
        assert_eq!(NOT_GRANTED, "Administrator rights were not granted.");
        let progress = Progress::default();
        assert_eq!(scan_indicator(&progress, Duration::ZERO), "Scanning.");
        progress.cancel.store(true, Ordering::Relaxed);
        assert_eq!(scan_indicator(&progress, Duration::ZERO), "Cancelling\u{2026}");
    }

    // Headless tests of `top_bar`: no window, no renderer, and the clock is passed in.
    mod ui {
        use super::*;
        use crate::ui::{ElevationHint, ScanOutcome};
        use eframe::egui::{Role, vec2};
        use egui_kittest::Harness;
        use egui_kittest::kittest::{NodeT, Queryable};
        use std::sync::{Arc, mpsc};

        struct State {
            app: App,
            now: Instant,
            last: Option<Action>,
            /// Stands for the scan thread, which has not ended yet.
            _scan: Option<mpsc::Sender<ScanOutcome>>,
        }

        fn harness(app: App, now: Instant, scan: Option<mpsc::Sender<ScanOutcome>>) -> Harness<'static, State> {
            Harness::builder()
                .with_size(vec2(1000.0, 200.0))
                .build_ui_state(
                |ui, s: &mut State| {
                    if let Some(action) = top_bar(ui, &mut s.app, s.now) {
                        if matches!(action, Action::Cancel) {
                            s.app.cancel_scan();
                        }
                        s.last = Some(action);
                    }
                },
                State { app, now, last: None, _scan: scan },
            )
        }

        /// A folder walk that has run for `elapsed_ms`.
        fn scanning(elapsed_ms: u64) -> Harness<'static, State> {
            let (tx, rx) = mpsc::channel();
            let started = Instant::now();
            let mut app = App::with_volumes(Vec::new());
            app.phase = Phase::Scanning {
                progress: Arc::new(Progress::default()),
                rx,
                started,
                method: ScanMethod::Walk,
                target: ScanTarget::Folder("x".into()),
            };
            harness(app, started + Duration::from_millis(elapsed_ms), Some(tx))
        }

        fn cancel_disabled(h: &Harness<'_, State>) -> bool {
            h.get_by_label("Cancel").accesskit_node().is_disabled()
        }

        fn cancel_requested(h: &Harness<'_, State>) -> bool {
            matches!(&h.state().app.phase, Phase::Scanning { progress, .. } if progress.cancel.load(Ordering::Relaxed))
        }

        #[test]
        fn running_scan_shows_dots_and_elapsed_time() {
            let mut h = scanning(600);
            h.run();
            assert!(h.query_by_label("Scanning...").is_some());
            assert!(h.query_by_label("0 entries found").is_some());
            assert!(h.query_by_label("0.6 s").is_some());
            assert!(!cancel_disabled(&h));
        }

        #[test]
        fn cancel_shows_cancelling_and_disables_the_button() {
            let mut h = scanning(600);
            h.run();
            h.get_by_label("Cancel").click();
            h.run();
            assert!(cancel_requested(&h));
            assert!(h.query_by_label("Cancelling\u{2026}").is_some());
            assert!(h.query_by_label("Scanning...").is_none());
            assert!(cancel_disabled(&h));
            // A second Cancel does nothing: the button is disabled and the state stays.
            h.state_mut().last = None;
            h.get_by_label("Cancel").click();
            h.run();
            assert!(h.state().last.is_none());
            assert!(cancel_requested(&h));
            assert!(h.query_by_label("Cancelling\u{2026}").is_some());
            assert!(cancel_disabled(&h));
        }

        fn with_hint(relaunch_failed: bool) -> Harness<'static, State> {
            let mut app = App::with_volumes(Vec::new());
            app.hint = Some(ElevationHint { letter: 'D', relaunch_failed });
            let mut h = harness(app, Instant::now(), None);
            h.run();
            h
        }

        #[test]
        fn hint_buttons_act_on_the_hint_drive() {
            let mut h = with_hint(false);
            assert!(h.query_by_label("The fast scan needs administrator rights.").is_some());
            assert!(h.query_by_label("Administrator rights were not granted.").is_none());
            h.get_by_label("Restart as administrator").click();
            h.run();
            assert!(matches!(h.state().last, Some(Action::Relaunch('D'))));
            h.get_by_label("Scan slowly").click();
            h.run();
            assert!(matches!(h.state().last, Some(Action::SlowScan(ScanTarget::Volume('D')))));
        }

        #[test]
        fn dots_do_not_move_the_count_beside_them() {
            let count_left = |elapsed_ms| {
                let mut h = scanning(elapsed_ms);
                h.run();
                h.get_by_label("0 entries found").rect().left()
            };
            let (one, two, three) = (count_left(0), count_left(250), count_left(500));
            assert_eq!(one, three);
            assert_eq!(two, three);
        }

        #[test]
        fn every_control_of_the_top_bar_has_a_name() {
            let volumes = vec![VolumeInfo { letter: 'C', fs_name: "NTFS".into(), total: 1 << 40, free: 1 << 39 }];
            let mut h = harness(App::with_volumes(volumes), Instant::now(), None);
            h.run();
            // The drive list: its name, and the chosen drive as its value.
            let list = h.get_by_role_and_label(Role::ComboBox, "Drive or folder to scan");
            assert_eq!(list.accesskit_node().value().as_deref(), Some("C:  NTFS  1.0 TB"));
            for button in ["Folder\u{2026}", "Scan"] {
                assert!(h.query_by_role_and_label(Role::Button, button).is_some(), "{button}");
            }
            // The `Show` choices: one group with two radio buttons.
            let group = h.get_by_role_and_label(Role::RadioGroup, "Show");
            let choices: Vec<_> = group.query_all_by_role(Role::RadioButton).map(|n| n.accesskit_node().label()).collect();
            assert_eq!(choices, [Some("Allocated".to_string()), Some("Size".to_string())]);
            // The treemap switch tells whether it is on.
            let switch = h.get_by_role_and_label(Role::Button, "Treemap");
            assert_eq!(switch.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
            // Picking a choice by its name works like a click.
            h.get_by_role_and_label(Role::RadioButton, "Size").click();
            h.run();
            assert!(matches!(h.state().last, Some(Action::MetricChanged)));
            assert!(h.state().app.metric == Metric::Size);
            h.get_by_role_and_label(Role::ComboBox, "Drive or folder to scan").click();
            h.run();
            assert!(matches!(h.state().last, Some(Action::DriveListOpened)));
            assert!(h.query_by_label("C:  NTFS  1.0 TB").is_some());
        }

        #[test]
        fn failed_relaunch_says_so() {
            let h = with_hint(true);
            assert!(h.query_by_label("The fast scan needs administrator rights.").is_some());
            assert!(h.query_by_label("Administrator rights were not granted.").is_some());
        }
    }
}
