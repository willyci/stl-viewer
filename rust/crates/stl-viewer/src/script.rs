//! `--script` debug steps: drive the real UI flows from the command line and log the status
//! bar after each step (used for end-to-end verification and screenshots).
//!
//! Steps are separated by `;`; arguments follow the first `:`:
//! `select:<path>` click a tree row (ancestors are revealed first) · `expand:<path>` toggle a
//! folder's expansion · `thumb:<name>` click a grid card · `next` · `prev` · `media:on|off` ·
//! `path:<text>` type a path + Enter · `pathblur:<text>` type a path + focus loss · `up` ·
//! `shading:<mode>` · `preset:<view>` · `rotate:on|off` · `color` open the color dialog ·
//! `colorok:#RRGGBB` accept a color · `copypath` click the Path link · `dismiss` close the
//! error dialog · `wait:<ms>` · `idle` wait until loads/renders settle · `shot:<file.png>` ·
//! `status` · `meta` · `tree` · `grid` · `page` · `frames` (frame times since the last
//! `frames`) · `quit`.
//!
//! Raw input, delivered through the real widgets (coordinates in points):
//! `click:x,y` · `dclick:x,y` · `move:x,y` · `wheel:x,y,lines` (positive scrolls down) ·
//! `drag:x0,y0,x1,y1` · `key:[ctrl+][shift+][alt+]<egui key name>` · `type:<text>`.

use std::path::PathBuf;

use stlv_render::{ShadingMode, ViewPreset};

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Select(PathBuf),
    Expand(PathBuf),
    Thumb(String),
    Next,
    Prev,
    Media(bool),
    Path {
        text: String,
        enter: bool,
    },
    Up,
    Shading(ShadingMode),
    Preset(ViewPreset),
    Rotate(bool),
    Color,
    ColorOk([u8; 3]),
    CopyPath,
    Dismiss,
    Wait(u64),
    Idle,
    Shot(PathBuf),
    Status,
    Meta,
    Click {
        pos: [f32; 2],
        double: bool,
    },
    Move([f32; 2]),
    Wheel {
        pos: [f32; 2],
        lines: f32,
    },
    Drag {
        from: [f32; 2],
        to: [f32; 2],
    },
    Key {
        key: egui::Key,
        modifiers: egui::Modifiers,
    },
    Type(String),
    Quit,
    Tree,
    Grid,
    Page,
    Frames,
}

pub fn parse(script: &str) -> Result<Vec<Step>, String> {
    script
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_step)
        .collect()
}

fn parse_step(s: &str) -> Result<Step, String> {
    let (name, arg) = match s.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a.trim())),
        None => (s, None),
    };
    let need = |what: &str| {
        arg.filter(|a| !a.is_empty())
            .ok_or_else(|| format!("script step `{name}` needs {what}"))
    };
    let on_off = |a: &str| match a {
        "on" | "1" | "true" => Ok(true),
        "off" | "0" | "false" => Ok(false),
        _ => Err(format!("script step `{name}`: expected on/off, got {a}")),
    };
    Ok(match name {
        "select" => Step::Select(need("a path")?.into()),
        "expand" => Step::Expand(need("a path")?.into()),
        "thumb" => Step::Thumb(need("a file name")?.into()),
        "next" => Step::Next,
        "prev" => Step::Prev,
        "media" => Step::Media(on_off(need("on/off")?)?),
        "path" => Step::Path {
            text: arg.unwrap_or_default().into(),
            enter: true,
        },
        "pathblur" => Step::Path {
            text: arg.unwrap_or_default().into(),
            enter: false,
        },
        "up" => Step::Up,
        "shading" => Step::Shading(need("a mode")?.parse()?),
        "preset" => Step::Preset(need("a view")?.parse()?),
        "rotate" => Step::Rotate(on_off(need("on/off")?)?),
        "color" => Step::Color,
        "colorok" => {
            Step::ColorOk(parse_hex(need("#RRGGBB")?).ok_or_else(|| format!("bad color {s}"))?)
        }
        "copypath" => Step::CopyPath,
        "dismiss" => Step::Dismiss,
        "wait" => Step::Wait(
            need("milliseconds")?
                .parse()
                .map_err(|_| format!("bad wait {s}"))?,
        ),
        "idle" => Step::Idle,
        "shot" => Step::Shot(need("a file")?.into()),
        "status" => Step::Status,
        "meta" => Step::Meta,
        "tree" => Step::Tree,
        "grid" => Step::Grid,
        "page" => Step::Page,
        "frames" => Step::Frames,
        "quit" => Step::Quit,
        "click" | "dclick" => {
            let [x, y] = numbers(need("x,y")?, s)?;
            Step::Click {
                pos: [x, y],
                double: name == "dclick",
            }
        }
        "move" => Step::Move(numbers(need("x,y")?, s)?),
        "wheel" => {
            let [x, y, lines] = numbers(need("x,y,lines")?, s)?;
            Step::Wheel { pos: [x, y], lines }
        }
        "drag" => {
            let [x0, y0, x1, y1] = numbers(need("x0,y0,x1,y1")?, s)?;
            Step::Drag {
                from: [x0, y0],
                to: [x1, y1],
            }
        }
        "key" => {
            let spec = need("a key name")?;
            // `ctrl+shift+A`; a lone `+` is the plus key.
            let (mods, k) = match spec.rsplit_once('+') {
                Some((m, k)) if !k.is_empty() => (m, k),
                _ => ("", spec),
            };
            let mut modifiers = egui::Modifiers::NONE;
            for m in mods.split('+').filter(|m| !m.is_empty()) {
                modifiers |= match m.to_ascii_lowercase().as_str() {
                    // egui-winit reports Ctrl as both `ctrl` and `command` off macOS.
                    "ctrl" => egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
                    "shift" => egui::Modifiers::SHIFT,
                    "alt" => egui::Modifiers::ALT,
                    _ => return Err(format!("unknown modifier {m} in `{s}`")),
                };
            }
            let key = egui::Key::from_name(k).ok_or_else(|| format!("unknown key {k}"))?;
            Step::Key { key, modifiers }
        }
        "type" => Step::Type(arg.unwrap_or_default().into()),
        _ => return Err(format!("unknown script step `{s}`")),
    })
}

/// Exactly `N` comma-separated numbers.
fn numbers<const N: usize>(arg: &str, step: &str) -> Result<[f32; N], String> {
    let v: Vec<f32> = arg
        .split(',')
        .map(|n| n.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("bad numbers in `{step}`"))?;
    v.try_into()
        .map_err(|_| format!("`{step}` needs {N} numbers"))
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps() {
        let steps = parse(r"select:C:\dev\a.stl; idle ;next;media:off;path:C:\x;pathblur:;shot:o.png;colorok:#00ff80")
            .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Select(r"C:\dev\a.stl".into()),
                Step::Idle,
                Step::Next,
                Step::Media(false),
                Step::Path {
                    text: r"C:\x".into(),
                    enter: true
                },
                Step::Path {
                    text: String::new(),
                    enter: false
                },
                Step::Shot("o.png".into()),
                Step::ColorOk([0, 255, 128]),
            ]
        );
        assert!(parse("bogus").is_err());
        assert!(parse("wait:soon").is_err());
        assert!(parse("select").is_err());
        assert!(parse("media:maybe").is_err());
        assert_eq!(
            parse(r"click:10,20;dclick:1,2;wheel:5,6,-3;drag:1,2,3,4;key:Enter;type:C:\x;quit")
                .unwrap(),
            vec![
                Step::Click {
                    pos: [10.0, 20.0],
                    double: false
                },
                Step::Click {
                    pos: [1.0, 2.0],
                    double: true
                },
                Step::Wheel {
                    pos: [5.0, 6.0],
                    lines: -3.0
                },
                Step::Drag {
                    from: [1.0, 2.0],
                    to: [3.0, 4.0]
                },
                Step::Key {
                    key: egui::Key::Enter,
                    modifiers: egui::Modifiers::NONE
                },
                Step::Type(r"C:\x".into()),
                Step::Quit,
            ]
        );
        assert!(parse("click:1").is_err());
        assert!(parse("key:Nope").is_err());
        assert_eq!(
            parse("key:ctrl+shift+A;key:+").unwrap(),
            vec![
                Step::Key {
                    key: egui::Key::A,
                    modifiers: egui::Modifiers::CTRL
                        | egui::Modifiers::COMMAND
                        | egui::Modifiers::SHIFT
                },
                Step::Key {
                    key: egui::Key::Plus,
                    modifiers: egui::Modifiers::NONE
                },
            ]
        );
        assert!(parse("key:meta+A").is_err());
    }
}
