#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    filebrowser::run()
}
