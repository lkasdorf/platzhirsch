#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

struct PlatzhirschApp;

impl eframe::App for PlatzhirschApp {
    fn ui(&mut self, _ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {}
}

fn main() -> eframe::Result {
    eframe::run_native(
        "Platzhirsch",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(PlatzhirschApp))),
    )
}
