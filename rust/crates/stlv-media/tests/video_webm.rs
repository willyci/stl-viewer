//! WebM playback through Media Foundation, driven by a headless egui context. The real
//! sample is played muted; the test is skipped when it is not present on this machine.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Harness, bright_fraction, temp_dir};
use stlv_media::VideoPlayer;

const SAMPLE: &str =
    r"C:\dev\Med2VR\playground\Assets\VRTemplateAssets\Videos\OnboardingVideoVRT.webm";
/// 960x700 window: 650 px video area, 1280x720 video aspect-fit to 960x540.
const FRAME_SIZE: [usize; 2] = [960, 540];

fn sample() -> Option<&'static Path> {
    let p = Path::new(SAMPLE);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipping: {SAMPLE} not found");
        None
    }
}

/// Run frames for `d` while showing the player.
fn idle(h: &mut Harness, v: &mut VideoPlayer, d: Duration) {
    let _ = h.run_until(v, d, VideoPlayer::show, |_| false);
}

#[test]
fn plays_pauses_seeks_ends_and_reopens() {
    let Some(path) = sample() else { return };
    let mut h = Harness::new(960.0, 700.0);
    let mut v = VideoPlayer::new();
    v.set_muted(true);
    let t = Instant::now();
    v.open(path).expect("open the WebM sample");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "open took {:?}",
        t.elapsed()
    );
    assert!(v.is_open() && v.is_playing() && v.is_muted());

    // Real frames arrive at the displayed size and are not black.
    assert!(
        h.run_until(&mut v, Duration::from_secs(5), VideoPlayer::show, |v| v
            .frames_shown()
            >= 10),
        "no frames"
    );
    assert_eq!(v.video_size(), Some([1280, 720]));
    assert_eq!(v.duration_ms(), Some(24_625));
    let frames = h.uploads_of_size(FRAME_SIZE);
    assert!(!frames.is_empty(), "no {FRAME_SIZE:?} uploads");
    assert!(
        frames.iter().any(|f| bright_fraction(f, 128) > 0.01),
        "only black frames"
    );
    let p0 = v.position_ms();
    idle(&mut h, &mut v, Duration::from_millis(500));
    assert!(
        v.position_ms() >= p0 + 300,
        "position did not advance: {p0} -> {}",
        v.position_ms()
    );

    // Pause: reflected immediately, position holds.
    v.pause();
    assert!(!v.is_playing());
    idle(&mut h, &mut v, Duration::from_millis(300));
    let held = v.position_ms();
    idle(&mut h, &mut v, Duration::from_millis(500));
    assert!(
        v.position_ms().abs_diff(held) <= 50,
        "moved while paused: {held} -> {}",
        v.position_ms()
    );

    // Seek while paused: new position at once, and a new frame of a different scene.
    let shown = v.frames_shown();
    v.seek_ms(12_000);
    assert_eq!(v.position_ms(), 12_000);
    assert!(
        h.run_until(&mut v, Duration::from_secs(3), VideoPlayer::show, |v| v
            .frames_shown()
            > shown),
        "no frame after seek"
    );
    idle(&mut h, &mut v, Duration::from_millis(300));
    assert!(
        v.position_ms().abs_diff(12_000) <= 100,
        "after seek: {}",
        v.position_ms()
    );
    assert!(!v.is_playing());
    let seeked = h.uploads.last().cloned().expect("frame");
    assert_eq!(seeked.size, FRAME_SIZE);
    assert!(
        bright_fraction(&seeked, 128) > 0.3,
        "seeked frame (bright 3D scene) looks dark"
    );

    // Mute toggles.
    v.set_muted(false);
    assert!(!v.is_muted());
    v.set_muted(true);
    assert!(v.is_muted());

    // Resume, then run to the end: playing stops and the button would read "Play".
    v.play();
    assert!(v.is_playing());
    idle(&mut h, &mut v, Duration::from_millis(400));
    assert!(v.position_ms() > 12_100);
    v.seek_ms(24_000);
    assert!(
        h.run_until(&mut v, Duration::from_secs(4), VideoPlayer::show, |v| v
            .is_ended()),
        "never ended"
    );
    assert!(!v.is_playing());
    // Play after the end restarts from the beginning.
    v.play();
    assert!(v.is_playing());
    assert!(
        h.run_until(&mut v, Duration::from_secs(3), VideoPlayer::show, |v| v
            .position_ms()
            < 5_000
            && !v.is_ended()),
        "did not restart: {} ms",
        v.position_ms()
    );

    // Stop releases everything; reopening works, repeatedly.
    let t = Instant::now();
    v.stop();
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "stop took {:?}",
        t.elapsed()
    );
    assert!(!v.is_open() && !v.is_playing());
    assert_eq!(
        (v.position_ms(), v.duration_ms(), v.frames_shown()),
        (0, None, 0)
    );
    idle(&mut h, &mut v, Duration::from_millis(100));
    for _ in 0..5 {
        v.open(path).expect("reopen");
        assert!(
            h.run_until(&mut v, Duration::from_secs(5), VideoPlayer::show, |v| v
                .frames_shown()
                >= 2),
            "no frames after reopen"
        );
        v.stop();
    }
    // Opening over an open file replaces it.
    v.open(path).expect("open");
    v.open(path).expect("open again");
    assert!(h.run_until(
        &mut v,
        Duration::from_secs(5),
        VideoPlayer::show,
        |v| v.frames_shown() >= 2
    ));
    drop(v);
}

#[test]
fn unplayable_files_are_errors() {
    let mut v = VideoPlayer::new();
    v.set_muted(true);
    let missing = v
        .open(Path::new(r"Z:\definitely\missing.webm"))
        .unwrap_err();
    assert!(!missing.is_empty() && !v.is_open(), "{missing}");

    let dir = temp_dir("video");
    let garbage = dir.join("garbage.webm");
    std::fs::write(&garbage, b"this is not a webm file at all".repeat(100)).unwrap();
    let err = v.open(&garbage).unwrap_err();
    assert!(err.contains("not a valid WebM"), "{err}");
    assert!(!v.is_open());
    let empty = dir.join("empty.webm");
    std::fs::write(&empty, b"").unwrap();
    assert!(v.open(&empty).is_err());

    // Passes the header check, so Media Foundation itself rejects it (message carries the
    // HRESULT).
    let fake = dir.join("fake.webm");
    let mut data = vec![0x1A, 0x45, 0xDF, 0xA3];
    data.extend(b"garbage after a valid EBML magic".repeat(100));
    std::fs::write(&fake, data).unwrap();
    let err = v.open(&fake).unwrap_err();
    eprintln!(
        "garbage.webm: ...
fake.webm: {err}
missing: {missing}"
    );
    assert!(err.contains("0x"), "{err}");
    assert!(!v.is_open());
}

#[test]
fn truncated_webm_does_not_crash() {
    let Some(path) = sample() else { return };
    let data = std::fs::read(path).unwrap();
    let dir = temp_dir("video-trunc");
    let cut = dir.join("cut.webm");
    std::fs::write(&cut, &data[..data.len() / 3]).unwrap();
    let mut h = Harness::new(640.0, 480.0);
    let mut v = VideoPlayer::new();
    v.set_muted(true);
    // Either it refuses to open or it plays what is there; neither may hang or panic.
    match v.open(&cut) {
        Ok(()) => {
            v.seek_ms(20_000);
            idle(&mut h, &mut v, Duration::from_millis(1500));
            eprintln!(
                "cut.webm: pos={} ended={} error={:?}",
                v.position_ms(),
                v.is_ended(),
                v.error()
            );
            v.stop();
        }
        Err(e) => eprintln!("cut.webm: {e}"),
    }
}

#[test]
fn webm_past_max_path_plays() {
    let Some(path) = sample() else { return };
    let root = temp_dir("video-long");
    let mut dir = root.to_path_buf();
    for i in 0..4 {
        dir.push(format!("d{i}_{}", "x".repeat(90)));
    }
    std::fs::create_dir_all(&dir).unwrap();
    let long = dir.join("clip.webm");
    assert!(long.as_os_str().len() > 300);
    std::fs::copy(path, &long).unwrap();
    let mut h = Harness::new(640.0, 480.0);
    let mut v = VideoPlayer::new();
    v.set_muted(true);
    v.open(&long).expect("open long path");
    assert!(
        h.run_until(&mut v, Duration::from_secs(5), VideoPlayer::show, |v| v
            .frames_shown()
            >= 2),
        "no frames from a long path: {:?}",
        v.error()
    );
    v.stop();
}
