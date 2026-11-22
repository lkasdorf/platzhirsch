//! The main window: top bar, central area, status bar and the scan lifecycle.
pub mod bars;
pub mod rows;
pub mod table;
pub mod treemap_view;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::model::Metric;
use crate::scan::mft::scan_mft;
use crate::scan::walk::scan_walk;
use crate::scan::{MethodChoice, Progress, ScanError, ScanMethod, ScanResult, ScanTarget, choose_method};
use crate::win::{self, VolumeInfo};
use bars::Action;
use rows::TreeView;
use treemap_view::TreemapCache;

const IDLE_HINT: &str = "Choose a drive and press \"Scan\".";
const SCAN_ENDED: &str = "The scan ended unexpectedly.";
/// How often the window redraws while a scan runs; also the step of the `Scanning` dots.
const SCAN_REPAINT: Duration = Duration::from_millis(250);
/// Height of the treemap area until the user drags the divider.
const TREEMAP_HEIGHT: f32 = 260.0;
const TREEMAP_MIN_HEIGHT: f32 = 60.0;
/// Room the divider always leaves for the table.
const TABLE_MIN_HEIGHT: f32 = 80.0;

/// What the scan thread sends back: the result and, for a volume, its fresh space figures.
type ScanOutcome = (Result<ScanResult, ScanError>, Option<VolumeInfo>);

pub enum Phase {
    Idle,
    Scanning {
        progress: Arc<Progress>,
        rx: Receiver<ScanOutcome>,
        started: Instant,
        method: ScanMethod,
        target: ScanTarget,
    },
    Done {
        result: Arc<ScanResult>,
        view: TreeView,
        /// Belongs to this result only: a new scan starts with an empty cache.
        /// Boxed: with its mesh and resize state it is the largest field.
        treemap: Box<TreemapCache>,
        /// Re-read after the scan when the target was a volume.
        volume: Option<VolumeInfo>,
    },
    Failed {
        message: String,
        offer_walk: Option<ScanTarget>,
    },
}

/// A whole NTFS volume was chosen without admin rights. Kept beside the phase, so a
/// result on screen stays there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ElevationHint {
    pub letter: char,
    /// The last "Restart as administrator" did not start the elevated copy.
    pub relaunch_failed: bool,
}

/// What the UI took from the scan thread's channel.
enum Arrival {
    /// The thread sent its outcome.
    Sent(Box<ScanOutcome>),
    /// The thread ended without sending anything, e.g. after a panic.
    Ended,
}

/// The state that follows a scan; [`App::poll_scan`] turns it into a [`Phase`].
enum NextPhase {
    Idle,
    Done(Box<ScanResult>, Option<VolumeInfo>),
    Failed { message: String, offer_walk: Option<ScanTarget> },
}

/// Decides what follows when something arrives from the scan thread. `method` and
/// `target` describe the scan that ran, `cancelled` whether Cancel was pressed.
/// After Cancel whatever was sent is dropped. A thread that ended without sending is
/// reported even then: that is a fault, not the user's choice. After the fast scan of a
/// volume, that offers the slow scan like any other fast-scan error.
fn after_scan(method: ScanMethod, target: &ScanTarget, cancelled: bool, arrival: Arrival) -> NextPhase {
    let outcome = match arrival {
        Arrival::Ended => {
            let offer_walk = match (method, target) {
                (ScanMethod::Mft, ScanTarget::Volume(_)) => Some(target.clone()),
                _ => None,
            };
            return NextPhase::Failed { message: SCAN_ENDED.into(), offer_walk };
        }
        Arrival::Sent(_) if cancelled => return NextPhase::Idle,
        Arrival::Sent(outcome) => *outcome,
    };
    match outcome {
        (Ok(result), info) => NextPhase::Done(Box::new(result), info),
        (Err(ScanError::Cancelled), _) => NextPhase::Idle,
        (Err(e), _) => match (method, target) {
            (ScanMethod::Mft, ScanTarget::Volume(_)) => {
                NextPhase::Failed { message: format!("Fast scan failed: {e}"), offer_walk: Some(target.clone()) }
            }
            _ => NextPhase::Failed { message: e.to_string(), offer_walk: None },
        },
    }
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

/// The text in the middle of an idle window. None under the elevation hint: the user
/// has just pressed Scan, and the hint row says what to do next.
fn idle_text(hint: Option<ElevationHint>) -> Option<&'static str> {
    hint.is_none().then_some(IDLE_HINT)
}

fn same_target(a: &ScanTarget, b: &ScanTarget) -> bool {
    match (a, b) {
        (ScanTarget::Volume(x), ScanTarget::Volume(y)) => x == y,
        (ScanTarget::Folder(x), ScanTarget::Folder(y)) => x == y,
        _ => false,
    }
}

/// Phase and hint after the selected target changes from `old` to `new`: the
/// elevation hint and an error belong to the old target and are dropped; a result
/// stays.
fn after_target_change(
    phase: Phase,
    hint: Option<ElevationHint>,
    old: Option<&ScanTarget>,
    new: &ScanTarget,
) -> (Phase, Option<ElevationHint>) {
    if old.is_some_and(|old| same_target(old, new)) {
        return (phase, hint);
    }
    match phase {
        Phase::Failed { .. } => (Phase::Idle, None),
        other => (other, None),
    }
}

pub(crate) struct App {
    volumes: Vec<VolumeInfo>,
    target: Option<ScanTarget>,
    metric: Metric,
    /// Shown below the table; used by the treemap view.
    show_treemap: bool,
    phase: Phase,
    hint: Option<ElevationHint>,
    /// Set while the folder dialog is open.
    folder_rx: Option<Receiver<Option<PathBuf>>>,
    /// Set while the drive list is re-read in the background.
    volumes_rx: Option<Receiver<Vec<VolumeInfo>>>,
    /// Whether the drive list was open in the last frame.
    drive_list_open: bool,
}

impl App {
    fn new() -> Self {
        Self::with_volumes(win::list_volumes())
    }

    /// An idle app with drive C (or the first drive) selected.
    fn with_volumes(volumes: Vec<VolumeInfo>) -> Self {
        let target = volumes
            .iter()
            .find(|v| v.letter == 'C')
            .or_else(|| volumes.first())
            .map(|v| ScanTarget::Volume(v.letter));
        App {
            volumes,
            target,
            metric: Metric::Allocated,
            show_treemap: true,
            phase: Phase::Idle,
            hint: None,
            folder_rx: None,
            volumes_rx: None,
            drive_list_open: false,
        }
    }

    /// Picks the method for the selected target and starts it, or shows the elevation hint.
    fn scan(&mut self, ctx: &egui::Context) {
        let Some(target) = self.target.clone() else { return };
        let fs_name = volume_letter(&target)
            .and_then(|letter| self.volumes.iter().find(|v| v.letter == letter))
            .map_or("", |v| v.fs_name.as_str());
        let choice = choose_method(&target, win::is_elevated(), fs_name);
        match (choice, volume_letter(&target)) {
            (MethodChoice::Mft, _) => self.start(ctx, ScanMethod::Mft, target),
            (MethodChoice::AskElevation, Some(letter)) => self.ask_elevation(letter),
            _ => self.start(ctx, ScanMethod::Walk, target),
        }
    }

    /// Shows the elevation hint for drive `letter`. A shown result stays; an error
    /// gives way to the hint.
    fn ask_elevation(&mut self, letter: char) {
        self.hint = Some(ElevationHint { letter, relaunch_failed: false });
        if matches!(self.phase, Phase::Failed { .. }) {
            self.phase = Phase::Idle;
        }
    }

    /// Starts the scan of `target` with `method`; the MFT scan needs a volume.
    fn start(&mut self, ctx: &egui::Context, method: ScanMethod, target: ScanTarget) {
        // The thread is not waited for: closing the window must not block on it.
        let _detached = match (method, volume_letter(&target)) {
            (ScanMethod::Mft, Some(letter)) => self.start_with(ctx, method, target, move |p| scan_mft(letter, p)),
            _ => {
                let path = walk_path(&target);
                self.start_with(ctx, ScanMethod::Walk, target, move |p| scan_walk(&path, p))
            }
        };
    }

    /// Runs `run` on its own thread; the outcome arrives over a channel. Clears the
    /// elevation hint.
    fn start_with<F>(&mut self, ctx: &egui::Context, method: ScanMethod, target: ScanTarget, run: F) -> JoinHandle<()>
    where
        F: FnOnce(&Progress) -> Result<ScanResult, ScanError> + Send + 'static,
    {
        self.hint = None;
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let thread_progress = Arc::clone(&progress);
        let volume = volume_letter(&target);
        let ctx = ctx.clone();
        let handle = thread::spawn(move || {
            let result = run(&thread_progress);
            let info = volume.and_then(win::volume_info);
            let _ = tx.send((result, info));
            ctx.request_repaint();
        });
        self.phase = Phase::Scanning { progress, rx, started: Instant::now(), method, target };
        handle
    }

    /// Asks the running scan to stop. Pressing it again changes nothing.
    fn cancel_scan(&self) {
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Takes over what the scan thread delivered; keeps redrawing while it runs.
    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Phase::Scanning { progress, rx, method, target, .. } = &self.phase else { return };
        let arrival = match rx.try_recv() {
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(SCAN_REPAINT);
                return;
            }
            Err(TryRecvError::Disconnected) => Arrival::Ended,
            Ok(outcome) => Arrival::Sent(Box::new(outcome)),
        };
        let cancelled = progress.cancel.load(Ordering::Relaxed);
        self.phase = match after_scan(*method, target, cancelled, arrival) {
            NextPhase::Idle => Phase::Idle,
            NextPhase::Done(result, info) => {
                let view = TreeView::new(&result.tree, self.metric);
                Phase::Done { result: Arc::new(*result), view, treemap: Box::default(), volume: info }
            }
            NextPhase::Failed { message, offer_walk } => Phase::Failed { message, offer_walk },
        };
    }

    fn set_target(&mut self, target: ScanTarget) {
        let phase = std::mem::replace(&mut self.phase, Phase::Idle);
        (self.phase, self.hint) = after_target_change(phase, self.hint, self.target.as_ref(), &target);
        self.target = Some(target);
    }

    /// Takes over the folder chosen in the dialog.
    fn poll_folder(&mut self) {
        let Some(rx) = &self.folder_rx else { return };
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            Ok(picked) => {
                if let Some(path) = picked {
                    self.set_target(ScanTarget::from_path(path));
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

    /// Re-reads the drive list on its own thread, because a volume query can wait for
    /// a drive to wake up. The old list stays until the new one arrives.
    fn refresh_volumes(&mut self, ctx: &egui::Context) {
        if self.volumes_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(win::list_volumes());
            ctx.request_repaint();
        });
        self.volumes_rx = Some(rx);
    }

    /// Takes over a re-read drive list.
    fn poll_volumes(&mut self) {
        let Some(rx) = &self.volumes_rx else { return };
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            Ok(volumes) => {
                self.volumes = volumes;
                self.volumes_rx = None;
            }
            Err(TryRecvError::Disconnected) => self.volumes_rx = None,
        }
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context, frame: &eframe::Frame) {
        match action {
            Action::DriveListOpened => self.refresh_volumes(ctx),
            Action::SelectVolume(letter) => self.set_target(ScanTarget::Volume(letter)),
            Action::PickFolder => self.pick_folder(ctx, frame),
            Action::Scan => self.scan(ctx),
            Action::Cancel => self.cancel_scan(),
            Action::MetricChanged => {
                if let Phase::Done { result, view, .. } = &mut self.phase {
                    view.set_metric(&result.tree, self.metric);
                }
            }
            Action::Relaunch(letter) => match win::relaunch_elevated(&["--scan".into(), format!("{letter}:")]) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(_) => {
                    if let Some(hint) = &mut self.hint {
                        hint.relaunch_failed = true;
                    }
                }
            },
            Action::SlowScan(target) => self.start(ctx, ScanMethod::Walk, target),
        }
    }

    /// The area between the bars: a hint, the scan indicator or the tree table with
    /// the treemap below it.
    fn central_area(&mut self, ui: &mut egui::Ui, now: Instant) {
        let show_treemap = self.show_treemap;
        let hint = self.hint;
        match &mut self.phase {
            Phase::Idle => {
                if let Some(text) = idle_text(hint) {
                    ui.centered_and_justified(|ui| ui.label(text));
                }
            }
            Phase::Scanning { progress, started, .. } => {
                let text = bars::scan_indicator(progress, now.saturating_duration_since(*started));
                ui.centered_and_justified(|ui| ui.label(text));
            }
            Phase::Done { result, view, treemap, .. } => {
                let tree = &result.tree;
                let mut scroll_to = None;
                if show_treemap {
                    // Drawn first so the table gets the rest of the height.
                    let clicked = egui::Panel::bottom("treemap_panel")
                        .resizable(true)
                        .default_size(TREEMAP_HEIGHT)
                        .min_size(TREEMAP_MIN_HEIGHT)
                        .max_size((ui.available_height() - TABLE_MIN_HEIGHT).max(TREEMAP_MIN_HEIGHT))
                        .frame(egui::Frame::NONE.inner_margin(egui::Margin { top: 4, ..egui::Margin::ZERO }))
                        .show(ui, |ui| treemap_view::show(ui, tree, view, treemap, now))
                        .inner;
                    if let Some(node) = clicked {
                        scroll_to = Some(view.reveal(tree, node));
                    }
                }
                table::show(ui, tree, view, scroll_to);
            }
            Phase::Failed { .. } => {}
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
        self.poll_volumes();
        let now = Instant::now();
        let action = egui::Panel::top("top_bar").show(ui, |ui| bars::top_bar(ui, self, now)).inner;
        egui::Panel::bottom("status_bar").show(ui, |ui| bars::status_bar(ui, &self.phase));
        egui::CentralPanel::default_margins().show(ui, |ui| self.central_area(ui, now));
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

/// The app icon, a simple pie chart (a share of a disk), drawn in code: `size` x `size`
/// pixels as RGBA, rows top to bottom. The window icon and `assets/platzhirsch.ico`
/// (embedded in the executable, see `tests/icon_file.rs`) both come from here.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    // Drawn for 32 pixels; other sizes scale the radii.
    let scale = size as f32 / 32.0;
    let (outer, ring) = (15.0 * scale, 13.5 * scale);
    let center = (size as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - center, y as f32 - center);
            let r = (dx * dx + dy * dy).sqrt();
            let pixel = if r > outer {
                [0, 0, 0, 0]
            } else if r > ring {
                [30, 60, 90, 255]
            } else if dx >= 0.0 && dy <= 0.0 {
                [240, 170, 50, 255]
            } else {
                [60, 140, 190, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

fn window_icon() -> egui::IconData {
    const N: u32 = 32;
    egui::IconData { rgba: icon_rgba(N), width: N, height: N }
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
    use std::path::Path;

    fn result() -> ScanResult {
        let mut b = TreeBuilder::with_capacity(1, 2);
        let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR);
        let (tree, _) = b.finish(root);
        ScanResult {
            tree,
            method: ScanMethod::Walk,
            duration: Duration::ZERO,
            skipped: 0,
            unreadable: 0,
            online_only: 0,
        }
    }

    fn done_phase() -> Phase {
        let result = result();
        let view = TreeView::new(&result.tree, Metric::Allocated);
        Phase::Done { result: Arc::new(result), view, treemap: Box::default(), volume: None }
    }

    const C: ScanTarget = ScanTarget::Volume('C');
    const F: ScanTarget = ScanTarget::Volume('F');

    fn hint(letter: char) -> Option<ElevationHint> {
        Some(ElevationHint { letter, relaunch_failed: false })
    }

    #[test]
    fn elevation_hint_is_dropped_for_another_drive() {
        let (phase, h) = after_target_change(Phase::Idle, hint('C'), Some(&C), &F);
        assert!(matches!(phase, Phase::Idle));
        assert_eq!(h, None);
    }

    #[test]
    fn error_is_dropped_when_a_folder_is_picked() {
        let failed = Phase::Failed { message: "x".into(), offer_walk: Some(ScanTarget::Volume('C')) };
        let (phase, h) = after_target_change(failed, None, Some(&C), &ScanTarget::Folder(r"D:\x".into()));
        assert!(matches!(phase, Phase::Idle));
        assert_eq!(h, None);
    }

    #[test]
    fn same_drive_again_keeps_the_hint() {
        let failed_relaunch = Some(ElevationHint { letter: 'C', relaunch_failed: true });
        let (phase, h) = after_target_change(done_phase(), failed_relaunch, Some(&C), &C);
        assert!(matches!(phase, Phase::Done { .. }));
        assert_eq!(h, failed_relaunch);
    }

    #[test]
    fn finished_scan_stays_when_another_drive_is_chosen() {
        let (phase, h) = after_target_change(done_phase(), hint('C'), Some(&C), &F);
        assert!(matches!(phase, Phase::Done { .. }));
        assert_eq!(h, None);
    }

    fn app() -> App {
        App::with_volumes(Vec::new())
    }

    fn app_with_picked_folder(picked: &str) -> App {
        let (tx, rx) = mpsc::channel();
        tx.send(Some(PathBuf::from(picked))).unwrap();
        let mut app = app();
        app.folder_rx = Some(rx);
        app.poll_folder();
        app
    }

    #[test]
    fn drive_root_from_folder_dialog_is_a_volume() {
        let app = app_with_picked_folder(r"D:\");
        assert!(matches!(app.target, Some(ScanTarget::Volume('D'))));
        assert!(app.folder_rx.is_none());
    }

    #[test]
    fn folder_from_folder_dialog_stays_a_folder() {
        let app = app_with_picked_folder(r"D:\photos");
        assert!(matches!(app.target, Some(ScanTarget::Folder(ref p)) if p == Path::new(r"D:\photos")));
    }

    #[test]
    fn elevation_hint_keeps_a_shown_result() {
        let mut app = app();
        app.phase = done_phase();
        app.ask_elevation('C');
        assert!(matches!(app.phase, Phase::Done { .. }));
        assert_eq!(app.hint, hint('C'));
    }

    #[test]
    fn elevation_hint_replaces_an_error() {
        let mut app = app();
        app.phase = Phase::Failed { message: "x".into(), offer_walk: None };
        app.ask_elevation('C');
        assert!(matches!(app.phase, Phase::Idle));
        assert_eq!(app.hint, hint('C'));
    }

    #[test]
    fn starting_a_scan_clears_the_hint() {
        let mut app = app();
        app.hint = Some(ElevationHint { letter: 'C', relaunch_failed: true });
        let ctx = egui::Context::default();
        let folder = ScanTarget::Folder("x".into());
        let thread = app.start_with(&ctx, ScanMethod::Walk, folder, |_| Err(ScanError::Cancelled));
        assert_eq!(app.hint, None);
        assert!(matches!(app.phase, Phase::Scanning { .. }));
        thread.join().unwrap();
    }

    #[test]
    fn a_new_drive_list_replaces_the_old_one() {
        let mut app = App::with_volumes(vec![VolumeInfo { letter: 'C', fs_name: "NTFS".into(), total: 1, free: 0 }]);
        let (tx, rx) = mpsc::channel();
        app.volumes_rx = Some(rx);
        app.poll_volumes();
        assert_eq!(app.volumes.len(), 1, "the old list stays until the new one arrives");
        assert!(app.volumes_rx.is_some());
        tx.send(vec![
            VolumeInfo { letter: 'C', fs_name: "NTFS".into(), total: 1, free: 0 },
            VolumeInfo { letter: 'E', fs_name: "exFAT".into(), total: 1, free: 0 },
        ])
        .unwrap();
        app.poll_volumes();
        assert_eq!(app.volumes.iter().map(|v| v.letter).collect::<Vec<_>>(), ['C', 'E']);
        assert!(app.volumes_rx.is_none());
    }

    // The decision "what arrived -> what follows" (Review Focus 4).

    fn sent(result: Result<ScanResult, ScanError>) -> Arrival {
        Arrival::Sent(Box::new((result, None)))
    }

    fn volume_c() -> ScanTarget {
        ScanTarget::Volume('C')
    }

    #[test]
    fn mft_failure_offers_the_slow_scan() {
        let next = after_scan(ScanMethod::Mft, &volume_c(), false, sent(Err(ScanError::AccessDenied)));
        assert!(matches!(next, NextPhase::Failed { ref message, offer_walk: Some(ScanTarget::Volume('C')) }
            if message == "Fast scan failed: Access denied (administrator rights required)"));
    }

    #[test]
    fn walk_failure_offers_nothing() {
        for target in [volume_c(), ScanTarget::Folder(r"D:\x".into())] {
            let next = after_scan(ScanMethod::Walk, &target, false, sent(Err(ScanError::FolderAccessDenied)));
            assert!(matches!(next, NextPhase::Failed { ref message, offer_walk: None } if message == "Access denied"));
        }
    }

    #[test]
    fn cancelled_scan_goes_idle() {
        for method in [ScanMethod::Mft, ScanMethod::Walk] {
            for cancelled in [false, true] {
                let next = after_scan(method, &volume_c(), cancelled, sent(Err(ScanError::Cancelled)));
                assert!(matches!(next, NextPhase::Idle));
            }
        }
    }

    #[test]
    fn success_is_done() {
        let next = after_scan(ScanMethod::Mft, &volume_c(), false, sent(Ok(result())));
        assert!(matches!(next, NextPhase::Done(..)));
    }

    #[test]
    fn whatever_arrives_after_cancel_is_dropped() {
        let next = after_scan(ScanMethod::Walk, &volume_c(), true, sent(Ok(result())));
        assert!(matches!(next, NextPhase::Idle));
        let next = after_scan(ScanMethod::Mft, &volume_c(), true, sent(Err(ScanError::AccessDenied)));
        assert!(matches!(next, NextPhase::Idle));
    }

    #[test]
    fn thread_ended_without_result_fails() {
        for cancelled in [false, true] {
            // After the fast scan of a volume, the slow scan is offered as after any other fast-scan error.
            let next = after_scan(ScanMethod::Mft, &volume_c(), cancelled, Arrival::Ended);
            assert!(matches!(next, NextPhase::Failed { ref message, offer_walk: Some(ScanTarget::Volume('C')) }
                if message == "The scan ended unexpectedly."));
            // After a walk there is nothing slower to offer.
            for target in [volume_c(), ScanTarget::Folder(r"D:\x".into())] {
                let next = after_scan(ScanMethod::Walk, &target, cancelled, Arrival::Ended);
                assert!(matches!(next, NextPhase::Failed { ref message, offer_walk: None }
                    if message == "The scan ended unexpectedly."));
            }
        }
    }

    /// An app in the middle of a walk; the sender stands for the scan thread.
    fn scanning_app() -> (App, mpsc::Sender<ScanOutcome>) {
        let (tx, rx) = mpsc::channel();
        let mut app = app();
        app.phase = Phase::Scanning {
            progress: Arc::new(Progress::default()),
            rx,
            started: Instant::now(),
            method: ScanMethod::Walk,
            target: volume_c(),
        };
        (app, tx)
    }

    #[test]
    fn cancel_twice_then_result_goes_idle_once() {
        let ctx = egui::Context::default();
        let (mut app, tx) = scanning_app();
        app.cancel_scan();
        app.poll_scan(&ctx);
        app.cancel_scan();
        app.poll_scan(&ctx);
        assert!(matches!(app.phase, Phase::Scanning { ref progress, .. } if progress.cancel.load(Ordering::Relaxed)));
        tx.send((Ok(result()), None)).unwrap();
        app.poll_scan(&ctx);
        assert!(matches!(app.phase, Phase::Idle));
        // A second result from the same scan finds nothing to change.
        let _ = tx.send((Ok(result()), None));
        app.poll_scan(&ctx);
        assert!(matches!(app.phase, Phase::Idle));
    }

    #[test]
    fn no_idle_text_under_the_elevation_hint() {
        assert_eq!(idle_text(None), Some("Choose a drive and press \"Scan\"."));
        assert_eq!(idle_text(hint('C')), None);
    }

    #[test]
    fn result_without_cancel_is_shown() {
        let ctx = egui::Context::default();
        let (mut app, tx) = scanning_app();
        tx.send((Ok(result()), None)).unwrap();
        app.poll_scan(&ctx);
        assert!(matches!(app.phase, Phase::Done { .. }));
    }

    #[test]
    fn panicking_scan_thread_fails_the_scan() {
        let ctx = egui::Context::default();
        let mut app = app();
        let thread = app.start_with(&ctx, ScanMethod::Walk, ScanTarget::Folder("x".into()), |_| {
            panic!("scan thread panics on purpose in this test")
        });
        assert!(thread.join().is_err(), "the thread panicked");
        app.poll_scan(&ctx);
        assert!(matches!(app.phase, Phase::Failed { ref message, offer_walk: None }
            if message == "The scan ended unexpectedly."));
    }
}
