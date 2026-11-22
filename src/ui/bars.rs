//! Top bar and status bar of the main window.
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui;

use super::{App, Phase};
use crate::format::{format_bytes, format_count};
use crate::model::Metric;
use crate::scan::{ScanMethod, ScanResult, ScanTarget};
use crate::win::VolumeInfo;

/// What the user asked for in the top bar; applied by [`App`] after drawing.
pub(super) enum Action {
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
/// progress and the elevation / error notice.
pub(super) fn top_bar(ui: &mut egui::Ui, app: &mut App) -> Option<Action> {
    let mut action = None;
    let scanning = matches!(app.phase, Phase::Scanning { .. });
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!scanning, |ui| {
            egui::ComboBox::from_id_salt("target")
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
        });
        if ui.add_enabled(!scanning && app.folder_rx.is_none(), egui::Button::new("Folder…")).clicked() {
            action = Some(Action::PickFolder);
        }
        if scanning {
            if ui.button("Cancel").clicked() {
                action = Some(Action::Cancel);
            }
        } else if ui.add_enabled(app.target.is_some(), egui::Button::new("Scan")).clicked() {
            action = Some(Action::Scan);
        }
        ui.separator();
        ui.label("Show:");
        let allocated = ui.radio_value(&mut app.metric, Metric::Allocated, "Allocated");
        let size = ui.radio_value(&mut app.metric, Metric::Size, "Size");
        if allocated.changed() || size.changed() {
            action = Some(Action::MetricChanged);
        }
        ui.separator();
        ui.toggle_value(&mut app.show_treemap, "Treemap");
        if let Phase::Scanning { progress, started, method, .. } = &app.phase {
            ui.separator();
            let done = progress.done.load(Ordering::Relaxed);
            match method {
                ScanMethod::Mft => {
                    let total = progress.total.load(Ordering::Relaxed);
                    let fraction = if total == 0 { 0.0 } else { (done as f64 / total as f64).min(1.0) as f32 };
                    ui.add(egui::ProgressBar::new(fraction).desired_width(200.0).show_percentage());
                }
                ScanMethod::Walk => {
                    ui.spinner();
                    ui.label(format!("{} entries found", format_count(done)));
                }
            }
            ui.weak(format!("{} s", format_seconds(started.elapsed())));
        }
    });
    match &app.phase {
        Phase::AskElevation(letter) => {
            ui.horizontal(|ui| {
                ui.label("The fast scan needs administrator rights.");
                if ui.button("Restart as administrator").clicked() {
                    action = Some(Action::Relaunch(*letter));
                }
                if ui.button("Scan slowly").clicked() {
                    action = Some(Action::SlowScan(ScanTarget::Volume(*letter)));
                }
            });
        }
        Phase::Failed { message, offer_walk } => {
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, message);
                if let Some(target) = offer_walk
                    && ui.button("Scan slowly").clicked()
                {
                    let target = match target {
                        ScanTarget::Volume(letter) => ScanTarget::Volume(*letter),
                        ScanTarget::Folder(path) => ScanTarget::Folder(path.clone()),
                    };
                    action = Some(Action::SlowScan(target));
                }
            });
        }
        _ => {}
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
}
