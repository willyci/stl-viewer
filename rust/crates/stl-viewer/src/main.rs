#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! STL Viewer: desktop viewer for STL, 3MF, OBJ, G-code, images and WebM (Rust port of the
//! PySide6 + VTK app).

mod app;
mod card_info;
mod cli;
mod collate;
mod dialogs;
mod emoji;
mod fonts;
mod grid;
mod icons;
mod loader;
mod metadata;
mod overlay;
mod script;
mod settings;
#[cfg(test)]
mod test_util;
mod text;
mod theme;
mod tree;
mod watch;
mod widgets;
mod zslider;

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(all(windows, not(debug_assertions)))]
    attach_parent_console();
    env_logger::init();
    let args = match cli::parse(std::env::args_os().skip(1)) {
        Ok(cli::Command::Run(args)) => *args,
        Ok(cli::Command::Help) => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("STLViewer: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let settings_path = settings::path_for(args.cache_db.as_deref());
    let saved = settings::Settings::load(settings_path.as_deref());
    let size = args
        .window_size
        .or(saved.window_size)
        .unwrap_or([1280.0, 800.0]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(text::WINDOW_TITLE)
            .with_inner_size(size)
            .with_min_inner_size([640.0, 420.0])
            .with_icon(app_icon()),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    let result = eframe::run_native(
        "STL Viewer",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::StlViewerApp::new(
                cc,
                args,
                saved,
                settings_path,
            )?))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            eprintln!("STLViewer: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A release build is a GUI-subsystem exe, which starts without a console: attach to the one
/// it was launched from (if any) so `--help`, CLI errors and logs reach it. Redirected
/// standard handles (pipes, files) are kept.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: takes no pointers; it just fails when the parent has no console.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// The renderer's setup (device features for VTK-like antialiasing) on the power-saving GPU
/// unless `WGPU_POWER_PREF` says otherwise. Windows runs the Qt/VTK app there by default, and
/// on hybrid-graphics laptops presenting from the discrete GPU is several times slower
/// (measured: ~8 vs ~30 frames/s for an empty eframe window).
fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    use egui_wgpu::wgpu::PowerPreference;
    let mut config = stlv_render::wgpu_configuration();
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup
        && PowerPreference::from_env().is_none()
    {
        setup.power_preference = PowerPreference::LowPower;
    }
    config
}

/// The window / taskbar icon (the exe's own icon is embedded by build.rs).
fn app_icon() -> egui::IconData {
    let png = include_bytes!("../assets/icon.png");
    match image::load_from_memory(png) {
        Ok(img) => {
            let img = img.into_rgba8();
            let (width, height) = img.dimensions();
            egui::IconData {
                rgba: img.into_raw(),
                width,
                height,
            }
        }
        Err(e) => {
            log::warn!("app icon: {e}");
            egui::IconData::default()
        }
    }
}
