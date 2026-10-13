//! Command-line options (spec §7) plus debug flags used for screenshots and scripted runs.

use std::ffi::OsString;
use std::path::PathBuf;

use stlv_render::{ShadingMode, ViewPreset};

pub const USAGE: &str = "\
Usage: STLViewer [PATH] [OPTIONS]

  PATH                       folder to browse, or a file to open (its folder becomes the root)

Options:
  --cache-db <FILE>          thumbnail cache database to use instead of the default one
  --screenshot <OUT.png>     save a screenshot once the UI has settled, then exit
  --screenshot-delay-ms <N>  minimum time before the screenshot (default 1500)
  --window-size <WxH>        initial inner window size in points (default 1280x800)

Debug options:
  --select <PATH>            select a file or folder in the browser tree at startup
  --show-media               start with \"Show Images & Media\" checked
  --shading <MODE>           smooth | flat | wireframe | points (applied after the first load)
  --preset <VIEW>            iso | top | bottom | front | back | left | right (after the first load)
  --auto-rotate              enable auto-rotate after the first load
  --color-picker             open the model color dialog
  --script <STEPS>           ';'-separated UI actions, logged to stdout (see script.rs)
  -h, --help                 print this help
";

pub const DEFAULT_SCREENSHOT_DELAY_MS: u64 = 1500;

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub path: Option<PathBuf>,
    pub cache_db: Option<PathBuf>,
    pub screenshot: Option<PathBuf>,
    pub screenshot_delay_ms: u64,
    pub window_size: Option<[f32; 2]>,
    pub select: Option<PathBuf>,
    pub show_media: bool,
    pub shading: Option<ShadingMode>,
    pub preset: Option<ViewPreset>,
    pub auto_rotate: bool,
    pub color_picker: bool,
    pub script: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            path: None,
            cache_db: None,
            screenshot: None,
            screenshot_delay_ms: DEFAULT_SCREENSHOT_DELAY_MS,
            window_size: None,
            select: None,
            show_media: false,
            shading: None,
            preset: None,
            auto_rotate: false,
            color_picker: false,
            script: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Run(Box<Args>),
    Help,
}

/// Parse the arguments after the program name. `--flag value` and `--flag=value` are accepted.
pub fn parse<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut out = Args::default();
    let mut it = args.into_iter();
    while let Some(raw) = it.next() {
        let Some(arg) = raw.to_str() else {
            // A non-UTF-8 positional path is still a valid path.
            set_path(&mut out, PathBuf::from(raw))?;
            continue;
        };
        if !arg.starts_with('-') || arg == "-" {
            set_path(&mut out, PathBuf::from(arg))?;
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(OsString::from(v))),
            _ => (arg, None),
        };
        let mut value = |name: &str| -> Result<OsString, String> {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "-h" | "--help" => return Ok(Command::Help),
            "--cache-db" => out.cache_db = Some(value(flag)?.into()),
            "--screenshot" => out.screenshot = Some(value(flag)?.into()),
            "--screenshot-delay-ms" => {
                let v = utf8(value(flag)?, flag)?;
                out.screenshot_delay_ms = v
                    .trim()
                    .parse()
                    .map_err(|_| format!("{flag}: not a number: {v}"))?;
            }
            "--window-size" => {
                let v = utf8(value(flag)?, flag)?;
                out.window_size =
                    Some(parse_size(&v).ok_or_else(|| format!("{flag}: expected WxH, got {v}"))?);
            }
            "--select" => out.select = Some(value(flag)?.into()),
            "--show-media" => out.show_media = true,
            "--shading" => {
                let v = utf8(value(flag)?, flag)?;
                out.shading = Some(v.parse().map_err(|e| format!("{flag}: {e}"))?);
            }
            "--preset" => {
                let v = utf8(value(flag)?, flag)?;
                out.preset = Some(v.parse().map_err(|e| format!("{flag}: {e}"))?);
            }
            "--auto-rotate" => out.auto_rotate = true,
            "--color-picker" => out.color_picker = true,
            "--script" => out.script = Some(utf8(value(flag)?, flag)?),
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    Ok(Command::Run(Box::new(out)))
}

fn set_path(out: &mut Args, path: PathBuf) -> Result<(), String> {
    if out.path.is_some() {
        return Err(format!("unexpected extra argument {}", path.display()));
    }
    out.path = Some(path);
    Ok(())
}

fn utf8(v: OsString, flag: &str) -> Result<String, String> {
    v.into_string()
        .map_err(|_| format!("{flag}: value is not valid UTF-8"))
}

fn parse_size(s: &str) -> Option<[f32; 2]> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let w: f32 = w.trim().parse().ok()?;
    let h: f32 = h.trim().parse().ok()?;
    (w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0).then_some([w, h])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Args, String> {
        match parse(args.iter().map(OsString::from))? {
            Command::Run(a) => Ok(*a),
            Command::Help => Err("help".into()),
        }
    }

    #[test]
    fn defaults() {
        let a = run(&[]).unwrap();
        assert_eq!(a, Args::default());
        assert_eq!(a.screenshot_delay_ms, 1500);
    }

    #[test]
    fn all_flags() {
        let a = run(&[
            r"C:\models",
            "--cache-db",
            "t.db",
            "--screenshot=out.png",
            "--screenshot-delay-ms",
            "250",
            "--window-size",
            "1024x700",
            "--select",
            r"C:\models\a.stl",
            "--show-media",
            "--shading",
            "wireframe",
            "--preset=top",
            "--auto-rotate",
            "--color-picker",
            "--script",
            "next;prev",
        ])
        .unwrap();
        assert_eq!(a.path, Some(PathBuf::from(r"C:\models")));
        assert_eq!(a.cache_db, Some(PathBuf::from("t.db")));
        assert_eq!(a.screenshot, Some(PathBuf::from("out.png")));
        assert_eq!(a.screenshot_delay_ms, 250);
        assert_eq!(a.window_size, Some([1024.0, 700.0]));
        assert_eq!(a.select, Some(PathBuf::from(r"C:\models\a.stl")));
        assert!(a.show_media && a.auto_rotate && a.color_picker);
        assert_eq!(a.shading, Some(ShadingMode::Wireframe));
        assert_eq!(a.preset, Some(ViewPreset::Top));
        assert_eq!(a.script.as_deref(), Some("next;prev"));
    }

    #[test]
    fn errors() {
        assert!(run(&["--bogus"]).is_err());
        assert!(run(&["--cache-db"]).is_err());
        assert!(run(&["--window-size", "big"]).is_err());
        assert!(run(&["--window-size", "0x10"]).is_err());
        assert!(run(&["--screenshot-delay-ms", "-3"]).is_err());
        assert!(run(&["--shading", "neon"]).is_err());
        assert!(run(&["a", "b"]).is_err());
        assert_eq!(parse(["--help"].map(OsString::from)), Ok(Command::Help));
    }
}
