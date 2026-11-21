//! The main window: top bar, central area, status bar and the scan lifecycle.
pub mod bars;
pub mod rows;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::format::format_bytes;
use crate::model::Metric;
use crate::scan::mft::scan_mft;
use crate::scan::walk::scan_walk;
use crate::scan::{MethodChoice, Progress, ScanError, ScanMethod, ScanResult, ScanTarget, choose_method};
use crate::win::{self, VolumeInfo};
use bars::Action;
use rows::TreeView;

const IDLE_HINT: &str = "Choose a drive and press \"Scan\".";
/// How often the window redraws while a scan runs.
const SCAN_REPAINT: Duration = Duration::from_millis(250);

/// What the scan thread sends back: the result and, for a volume, its fresh space figures.
type ScanOutcome = (Result<ScanResult, ScanError>, Option<VolumeInfo>);

pub enum Phase {
    Idle,
    /// A whole NTFS volume was chosen without admin rights.
    AskElevation(char),
    Scanning {
        progress: Arc<Progress>,
        rx: Receiver<ScanOutcome>,
        started: Instant,
        method: ScanMethod,
        /// The drive letter if a whole volume is scanned.
        volume: Option<char>,
    },
    Done {
        result: Arc<ScanResult>,
        view: TreeView,
        /// Re-read after the scan when the target was a volume.
        volume: Option<VolumeInfo>,
    },
    Failed {
        message: String,
        offer_walk: Option<ScanTarget>,
    },
}

/// One scan to run in the background.
enum Job {
    Mft(char),
    Walk(PathBuf),
}

/// The path the walk starts at; a drive root is `X:\`.
fn walk_path(target: &ScanTarget) -> PathBuf {
    match target {
        ScanTarget::Volume(letter) => PathBuf::from(format!("{letter}:\\")),
        ScanTarget::Folder(path) => path.clone(),
    }
}

fn volume_letter(target: &ScanTarget) -> Option<char> {
    match target {
        ScanTarget::Volume(letter) => Some(*letter),
        ScanTarget::Folder(_) => None,
    }
}

fn same_target(a: &ScanTarget, b: &ScanTarget) -> bool {
    match (a, b) {
        (ScanTarget::Volume(x), ScanTarget::Volume(y)) => x == y,
        (ScanTarget::Folder(x), ScanTarget::Folder(y)) => x == y,
        _ => false,
    }
}

/// The phase after the selected target changes from `old` to `new`: the elevation
/// hint and an error belong to the old target and go back to `Idle`.
fn after_target_change(phase: Phase, old: Option<&ScanTarget>, new: &ScanTarget) -> Phase {
    let changed = !old.is_some_and(|old| same_target(old, new));
    match phase {
        Phase::AskElevation(_) | Phase::Failed { .. } if changed => Phase::Idle,
        other => other,
    }
}

pub(crate) struct App {
    volumes: Vec<VolumeInfo>,
    target: Option<ScanTarget>,
    metric: Metric,
    /// Shown below the table; used by the treemap view.
    show_treemap: bool,
    phase: Phase,
    /// Set while the folder dialog is open.
    folder_rx: Option<Receiver<Option<PathBuf>>>,
}

impl App {
    fn new() -> Self {
        let volumes = win::list_volumes();
        let target = volumes
            .iter()
            .find(|v| v.letter == 'C')
            .or_else(|| volumes.first())
            .map(|v| ScanTarget::Volume(v.letter));
        App { volumes, target, metric: Metric::Allocated, show_treemap: true, phase: Phase::Idle, folder_rx: None }
    }

    /// Picks the method for the selected target and starts it, or asks for elevation.
    fn scan(&mut self, ctx: &egui::Context) {
        let Some(target) = &self.target else { return };
        let fs_name = volume_letter(target)
            .and_then(|letter| self.volumes.iter().find(|v| v.letter == letter))
            .map_or("", |v| v.fs_name.as_str());
        match (choose_method(target, win::is_elevated(), fs_name), target) {
            (MethodChoice::Mft, ScanTarget::Volume(letter)) => {
                let letter = *letter;
                self.start(ctx, Job::Mft(letter), Some(letter));
            }
            (MethodChoice::AskElevation, ScanTarget::Volume(letter)) => self.phase = Phase::AskElevation(*letter),
            _ => {
                let volume = volume_letter(target);
                self.start(ctx, Job::Walk(walk_path(target)), volume);
            }
        }
    }

    /// Runs `job` on its own thread; the result arrives over a channel.
    fn start(&mut self, ctx: &egui::Context, job: Job, volume: Option<char>) {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let method = match job {
            Job::Mft(_) => ScanMethod::Mft,
            Job::Walk(_) => ScanMethod::Walk,
        };
        let thread_progress = Arc::clone(&progress);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = match &job {
                Job::Mft(letter) => scan_mft(*letter, &thread_progress),
                Job::Walk(path) => scan_walk(path, &thread_progress),
            };
            let info = volume.and_then(win::volume_info);
            let _ = tx.send((result, info));
            ctx.request_repaint();
        });
        self.phase = Phase::Scanning { progress, rx, started: Instant::now(), method, volume };
    }

    fn cancel_scan(&self) {
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Takes over a finished scan; keeps redrawing while it runs.
    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Phase::Scanning { rx, method, volume, .. } = &self.phase else { return };
        let (method, volume) = (*method, *volume);
        let received = rx.try_recv();
        self.phase = match received {
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(SCAN_REPAINT);
                return;
            }
            Err(TryRecvError::Disconnected) => {
                Phase::Failed { message: "The scan ended unexpectedly.".into(), offer_walk: None }
            }
            Ok((Ok(result), info)) => {
                let view = TreeView::new(&result.tree, self.metric);
                Phase::Done { result: Arc::new(result), view, volume: info }
            }
            Ok((Err(ScanError::Cancelled), _)) => Phase::Idle,
            Ok((Err(e), _)) => match (method, volume) {
                (ScanMethod::Mft, Some(letter)) => Phase::Failed {
                    message: format!("Fast scan failed: {e}"),
                    offer_walk: Some(ScanTarget::Volume(letter)),
                },
                _ => Phase::Failed { message: e.to_string(), offer_walk: None },
            },
        };
    }

    fn set_target(&mut self, target: ScanTarget) {
        let phase = std::mem::replace(&mut self.phase, Phase::Idle);
        self.phase = after_target_change(phase, self.target.as_ref(), &target);
        self.target = Some(target);
    }

    /// Takes over the folder chosen in the dialog.
    fn poll_folder(&mut self) {
        let Some(rx) = &self.folder_rx else { return };
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            Ok(picked) => {
                if let Some(path) = picked {
                    self.set_target(ScanTarget::Folder(path));
                }
                self.folder_rx = None;
            }
            Err(TryRecvError::Disconnected) => self.folder_rx = None,
        }
    }

    /// Opens the folder dialog on its own thread so the window keeps drawing.
    fn pick_folder(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let dialog = rfd::FileDialog::new().set_parent(frame);
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(dialog.pick_folder());
            ctx.request_repaint();
        });
        self.folder_rx = Some(rx);
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context, frame: &eframe::Frame) {
        match action {
            Action::SelectVolume(letter) => self.set_target(ScanTarget::Volume(letter)),
            Action::PickFolder => self.pick_folder(ctx, frame),
            Action::Scan => self.scan(ctx),
            Action::Cancel => self.cancel_scan(),
            Action::MetricChanged => {
                if let Phase::Done { result, view, .. } = &mut self.phase {
                    view.set_metric(&result.tree, self.metric);
                }
            }
            Action::Relaunch(letter) => {
                if win::relaunch_elevated(&["--scan".into(), format!("{letter}:")]).is_ok() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Action::SlowScan(target) => {
                let volume = volume_letter(&target);
                self.start(ctx, Job::Walk(walk_path(&target)), volume);
            }
        }
    }

    /// The area between the bars. For now the root and its total;
    /// the table and the treemap replace this.
    fn central_area(&mut self, ui: &mut egui::Ui) {
        match &self.phase {
            Phase::Idle => {
                ui.centered_and_justified(|ui| ui.label(IDLE_HINT));
            }
            Phase::Scanning { .. } => {
                ui.centered_and_justified(|ui| ui.spinner());
            }
            Phase::Done { result, view, .. } => {
                let tree = &result.tree;
                let root = tree.root();
                ui.heading(tree.name(root));
                ui.label(format_bytes(tree.metric(root, view.metric())));
            }
            Phase::AskElevation(_) | Phase::Failed { .. } => {}
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.cancel_scan();
        }
        self.poll_scan(&ctx);
        self.poll_folder();
        let action = egui::Panel::top("top_bar").show(ui, |ui| bars::top_bar(ui, self)).inner;
        egui::Panel::bottom("status_bar").show(ui, |ui| bars::status_bar(ui, &self.phase));
        egui::CentralPanel::default_margins().show(ui, |ui| self.central_area(ui));
        if let Some(action) = action {
            self.apply(action, &ctx, frame);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.cancel_scan();
    }
}

/// A simple pie-chart icon (a share of a disk), drawn in code.
fn window_icon() -> egui::IconData {
    const N: u32 = 32;
    let center = (N as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - center, y as f32 - center);
            let r = (dx * dx + dy * dy).sqrt();
            let pixel = if r > 15.0 {
                [0, 0, 0, 0]
            } else if r > 13.5 {
                [30, 60, 90, 255]
            } else if dx >= 0.0 && dy <= 0.0 {
                [240, 170, 50, 255]
            } else {
                [60, 140, 190, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    egui::IconData { rgba, width: N, height: N }
}

/// Opens the main window. With `initial`, its scan starts right away.
pub fn run(initial: Option<ScanTarget>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Platzhirsch")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Platzhirsch",
        options,
        Box::new(move |cc| {
            let mut app = App::new();
            if let Some(target) = initial {
                app.target = Some(target);
                app.scan(&cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{F_DIR, NO_PARENT, TreeBuilder};

    fn done_phase() -> Phase {
        let mut b = TreeBuilder::with_capacity(1, 2);
        let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR);
        let (tree, _) = b.finish(root);
        let view = TreeView::new(&tree, Metric::Allocated);
        let result = ScanResult {
            tree,
            method: ScanMethod::Walk,
            duration: Duration::ZERO,
            skipped: 0,
            unreadable: 0,
            online_only: 0,
        };
        Phase::Done { result: Arc::new(result), view, volume: None }
    }

    #[test]
    fn elevation_hint_is_dropped_for_another_drive() {
        let next = after_target_change(Phase::AskElevation('C'), Some(&ScanTarget::Volume('C')), &ScanTarget::Volume('F'));
        assert!(matches!(next, Phase::Idle));
    }

    #[test]
    fn error_is_dropped_when_a_folder_is_picked() {
        let failed = Phase::Failed { message: "x".into(), offer_walk: Some(ScanTarget::Volume('C')) };
        let next = after_target_change(failed, Some(&ScanTarget::Volume('C')), &ScanTarget::Folder(r"D:\x".into()));
        assert!(matches!(next, Phase::Idle));
    }

    #[test]
    fn same_drive_again_keeps_the_hint() {
        let next = after_target_change(Phase::AskElevation('C'), Some(&ScanTarget::Volume('C')), &ScanTarget::Volume('C'));
        assert!(matches!(next, Phase::AskElevation('C')));
    }

    #[test]
    fn finished_scan_stays_when_another_drive_is_chosen() {
        let next = after_target_change(done_phase(), Some(&ScanTarget::Volume('C')), &ScanTarget::Volume('F'));
        assert!(matches!(next, Phase::Done { .. }));
    }
}
