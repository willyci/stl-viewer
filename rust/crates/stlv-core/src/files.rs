use std::path::{Path, PathBuf};

/// 3D formats parsed by [`crate::load_model`].
pub const MESH_EXTS: &[&str] = &["stl", "3mf", "obj", "gcode", "gco"];
/// Image and video formats, shown only when "Show Images & Media" is enabled.
pub const MEDIA_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "tiff", "tif", "svg", "bmp", "webm",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Stl,
    ThreeMf,
    Obj,
    Gcode,
    /// Static raster image: jpg, jpeg, png, bmp, tiff, tif.
    Image,
    Gif,
    Svg,
    Webm,
    Other,
}

impl FileKind {
    pub fn from_path(path: &Path) -> Self {
        match ext_lower(path).as_str() {
            "stl" => Self::Stl,
            "3mf" => Self::ThreeMf,
            "obj" => Self::Obj,
            "gcode" | "gco" => Self::Gcode,
            "jpg" | "jpeg" | "png" | "bmp" | "tiff" | "tif" => Self::Image,
            "gif" => Self::Gif,
            "svg" => Self::Svg,
            "webm" => Self::Webm,
            _ => Self::Other,
        }
    }

    pub fn is_mesh(self) -> bool {
        matches!(self, Self::Stl | Self::ThreeMf | Self::Obj | Self::Gcode)
    }

    pub fn is_media(self) -> bool {
        matches!(self, Self::Image | Self::Gif | Self::Svg | Self::Webm)
    }
}

/// Lower-cased extension without the dot ("" when there is none).
pub fn ext_lower(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Whether a file should be listed: meshes always, media only when `show_media`.
pub fn is_supported(path: &Path, show_media: bool) -> bool {
    let kind = FileKind::from_path(path);
    kind.is_mesh() || (show_media && kind.is_media())
}

/// Supported files directly inside `dir`, sorted by case-insensitive file name.
pub fn list_supported_files(dir: &Path, show_media: bool) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| is_supported(p, show_media))
        .collect();
    files.sort_by_cached_key(|p| sort_key(p));
    Ok(files)
}

/// Case-insensitive file-name key used for every file ordering in the app.
pub fn sort_key(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_case_insensitively() {
        assert_eq!(FileKind::from_path(Path::new("a/B.STL")), FileKind::Stl);
        assert_eq!(FileKind::from_path(Path::new("x.GcO")), FileKind::Gcode);
        assert_eq!(FileKind::from_path(Path::new("x.tif")), FileKind::Image);
        assert_eq!(FileKind::from_path(Path::new("noext")), FileKind::Other);
        assert!(is_supported(Path::new("m.3mf"), false));
        assert!(!is_supported(Path::new("m.png"), false));
        assert!(is_supported(Path::new("m.png"), true));
    }
}
