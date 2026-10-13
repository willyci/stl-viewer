//! Dark slate theme colors (ported from styles.py / app.py) as `0xRRGGBB`.
//! GUI crates convert with e.g. `egui::Color32::from_rgb` via [`rgb`].

pub const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

// Accent
pub const ACCENT: u32 = 0x00F0FF; // cyan: group titles, hover borders, progress chunks
pub const ACCENT_BUTTON: u32 = 0x00BCCF; // "Model Color" button / checked checkbox
pub const ACCENT_PRESSED: u32 = 0x0093A3;
pub const LINK: u32 = 0x00C9D4; // clickable path label

// Surfaces
pub const WINDOW_BG: u32 = 0x0E1015;
pub const SIDEBAR_BG: u32 = 0x151821; // sidebar, control deck, video controls
pub const PANEL_BG: u32 = 0x11131A; // group boxes, tree, path edit, progress bar
pub const VIEWER_BG: u32 = 0x0A0C10; // image viewer, thumbnail grid, status bar
pub const CARD_BG: u32 = 0x1A1D26;
pub const CARD_HOVER_BG: u32 = 0x232836;
pub const THUMB_WELL_BG: u32 = 0x0D0F14; // inner preview area of a thumbnail card
pub const BUTTON_BG: u32 = 0x1E2333;
pub const BUTTON_HOVER_BG: u32 = 0x293147;
pub const BUTTON_PRESSED_BG: u32 = 0x151A26;
pub const TREE_HOVER_BG: u32 = 0x1C202D;
pub const TREE_SELECTED_BG: u32 = 0x163640;
pub const LOADING_CARD_BG: u32 = 0x161923;
pub const LOADING_TRACK: u32 = 0x232836;

// Borders
pub const BORDER: u32 = 0x232836; // group boxes, sidebar edge, cards, separators
pub const BORDER_SUBTLE: u32 = 0x1E222D; // tree, path edit
pub const BUTTON_BORDER: u32 = 0x2A3147;
pub const SPLITTER: u32 = 0x1F2432;
pub const SCROLL_TRACK: u32 = 0x1A1D28;
pub const SCROLL_HANDLE: u32 = 0x4A5578;

// Text
pub const TEXT: u32 = 0xA4ADC4; // labels
pub const TEXT_STRONG: u32 = 0xFFFFFF; // header, metadata values
pub const TEXT_BRIGHT: u32 = 0xE2E6F0; // tree items, buttons, combo
pub const TEXT_CARD: u32 = 0xE2E8F0; // card names, loading text
pub const TEXT_MUTED: u32 = 0x7A859E; // path edit, time label, progress text

/// Loading overlay veil: rgba(10, 12, 16, 184) — 72% opaque.
pub const OVERLAY_RGBA: [u8; 4] = [10, 12, 16, 184];

// Placeholder icon tints for thumbnail cards.
pub const ICON_STL: u32 = 0x06B6D4;
pub const ICON_3MF: u32 = 0xEAB308;
pub const ICON_OBJ: u32 = 0xF97316;
pub const ICON_GCODE: u32 = 0x22C55E;
pub const ICON_IMAGE: u32 = 0x7A859E;
pub const ICON_GIF: u32 = 0xA855F7;
pub const ICON_VIDEO: u32 = 0xF43F5E;

// 3D viewport (linear 0..1 floats, as configured on the VTK renderer).
pub const VIEWPORT_BG_BOTTOM: [f32; 3] = [0.07, 0.08, 0.11];
pub const VIEWPORT_BG_TOP: [f32; 3] = [0.15, 0.17, 0.22];
pub const DEFAULT_MODEL_COLOR: [f32; 3] = [0.0, 0.94, 1.0];
pub const GRID_COLOR: [f32; 3] = [0.28, 0.33, 0.44];
pub const GRID_OPACITY: f32 = 0.35;
pub const AXIS_LABEL_X: [f32; 3] = [1.0, 0.35, 0.35];
pub const AXIS_LABEL_Y: [f32; 3] = [0.35, 1.0, 0.35];
pub const AXIS_LABEL_Z: [f32; 3] = [0.35, 0.75, 1.0];
