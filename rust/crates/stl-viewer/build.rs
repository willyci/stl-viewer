//! Embeds `assets/icon.ico` as the executable's icon (what Explorer and the taskbar show).
//!
//! No resource compiler is needed: the icon is written straight into a Windows `.res` file,
//! which both LLVM's lld (used for windows-gnu, see `.cargo/config.toml`) and MSVC's
//! link.exe accept as a linker input.

use std::path::PathBuf;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = std::fs::read("assets/icon.ico").expect("read assets/icon.ico");
    let res = ico_to_res(&ico).expect("assets/icon.ico is not a valid .ico file");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icon.res");
    std::fs::write(&out, res).expect("write icon.res");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// One `.res` resource entry with numeric type and name.
fn push_resource(res: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    let le16 = |v: u16| v.to_le_bytes();
    let le32 = |v: u32| v.to_le_bytes();
    res.extend(le32(data.len() as u32)); // DataSize
    res.extend(le32(32)); // HeaderSize
    res.extend(le16(0xFFFF));
    res.extend(le16(kind)); // TYPE (ordinal)
    res.extend(le16(0xFFFF));
    res.extend(le16(id)); // NAME (ordinal)
    res.extend(le32(0)); // DataVersion
    res.extend(le16(flags)); // MemoryFlags
    res.extend(le16(LANG_EN_US)); // LanguageId
    res.extend(le32(0)); // Version
    res.extend(le32(0)); // Characteristics
    res.extend_from_slice(data);
    while !res.len().is_multiple_of(4) {
        res.push(0);
    }
}

fn ico_to_res(ico: &[u8]) -> Option<Vec<u8>> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(ico.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(ico.get(o..o + 4)?.try_into().ok()?));
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = u16_at(4)?;
    // The leading empty entry every .res file starts with.
    let mut res = Vec::new();
    push_resource(&mut res, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend(count.to_le_bytes());
    for i in 0..count {
        let e = 6 + 16 * usize::from(i);
        let (size, offset) = (u32_at(e + 8)? as usize, u32_at(e + 12)? as usize);
        let image = ico.get(offset..offset + size)?;
        let id = i + 1;
        push_resource(&mut res, RT_ICON, id, 0x1010, image);
        // GRPICONDIRENTRY: the ICONDIRENTRY's first 12 bytes, then the resource id.
        group.extend_from_slice(ico.get(e..e + 12)?);
        group.extend(id.to_le_bytes());
    }
    push_resource(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    Some(res)
}
