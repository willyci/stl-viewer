//! wgpu renderer for meshes and G-code toolpaths (port of renderer.py's VTK pipeline):
//! an interactive egui viewport and a thread-safe off-screen thumbnail renderer.

pub mod camera;
mod gpu;
mod marker;
mod prepared;
mod thumbnail;
mod viewport;

pub use camera::{Camera, ViewPreset};
pub use prepared::PreparedModel;
pub use thumbnail::{SnapshotOptions, THUMBNAIL_SIZE, ThumbnailRenderer};
pub use viewport::Viewport3D;

/// Optional device features the renderer benefits from, if `adapter` offers them
/// (`TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` enables VTK's 8x MSAA instead of 4x).
pub fn optional_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
}

/// Allocator hint for the renderer's devices. The default (`Performance`) reserves GPU memory
/// in 128-256 MB blocks, which on integrated GPUs is committed system RAM; a viewer with a few
/// large render targets loses nothing with the small blocks.
pub const MEMORY_HINTS: wgpu::MemoryHints = wgpu::MemoryHints::MemoryUsage;

/// egui-wgpu's default setup plus [`optional_features`] and [`MEMORY_HINTS`] (see
/// [`wgpu_configuration`]).
pub fn wgpu_setup() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    let base = setup.device_descriptor.clone();
    setup.device_descriptor = std::sync::Arc::new(move |adapter| {
        let mut desc = base(adapter);
        desc.required_features |= optional_features(adapter);
        desc.memory_hints = MEMORY_HINTS;
        desc
    });
    egui_wgpu::WgpuSetup::CreateNew(setup)
}

/// egui-wgpu's default configuration using [`wgpu_setup`]: pass it as
/// `eframe::NativeOptions::wgpu_options` so [`Viewport3D`] (and a [`ThumbnailRenderer`]
/// sharing the device) get VTK's 8x antialiasing; with eframe's defaults they use 4x.
pub fn wgpu_configuration() -> egui_wgpu::WgpuConfiguration {
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: wgpu_setup(),
        ..Default::default()
    }
}

/// Surface representation, mirroring the "Shading" combo box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ShadingMode {
    #[default]
    Smooth,
    Flat,
    Wireframe,
    Points,
}

impl ShadingMode {
    pub const ALL: [Self; 4] = [Self::Smooth, Self::Flat, Self::Wireframe, Self::Points];

    pub fn label(self) -> &'static str {
        match self {
            Self::Smooth => "Smooth Shading",
            Self::Flat => "Flat Shading",
            Self::Wireframe => "Wireframe",
            Self::Points => "Point Cloud",
        }
    }

    /// The combo box's item key (Python `set_shading_mode`): `smooth`, `flat`, `wireframe`
    /// or `points`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Smooth => "smooth",
            Self::Flat => "flat",
            Self::Wireframe => "wireframe",
            Self::Points => "points",
        }
    }

    /// Parses a [`name`](Self::name) or [`label`](Self::label), or the aliases `wire`,
    /// `point` and `pointcloud` (any case, surrounding whitespace ignored).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "smooth" | "smooth shading" => Self::Smooth,
            "flat" | "flat shading" => Self::Flat,
            "wireframe" | "wire" => Self::Wireframe,
            "points" | "point" | "pointcloud" | "point cloud" => Self::Points,
            _ => return None,
        })
    }
}

/// [`ShadingMode::from_name`] with an error message naming the accepted values.
impl std::str::FromStr for ShadingMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Self::from_name(s).ok_or_else(|| {
            format!("unknown shading mode {s:?} (expected smooth, flat, wireframe or points)")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shading_names_round_trip() {
        for m in ShadingMode::ALL {
            assert_eq!(ShadingMode::from_name(m.name()), Some(m));
            assert_eq!(ShadingMode::from_name(m.label()), Some(m));
            assert_eq!(m.name().parse::<ShadingMode>(), Ok(m));
        }
        assert_eq!(
            ShadingMode::from_name(" Wire "),
            Some(ShadingMode::Wireframe)
        );
        assert_eq!(
            ShadingMode::from_name("POINTCLOUD"),
            Some(ShadingMode::Points)
        );
        assert_eq!(ShadingMode::from_name(""), None);
        let err = "glossy".parse::<ShadingMode>().unwrap_err();
        assert!(
            err.contains("\"glossy\"") && err.contains("wireframe"),
            "{err}"
        );
    }
}
