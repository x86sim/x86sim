//! Device builds on a simulator phone: an .ipa, an .xcarchive or a device .app becomes a simulator .app that
//! `apps::install` installs like any other (simctl install takes only simulator builds; Apple's simulator refuses a
//! device binary at exec: "built for iOS" vs "iOS Simulator").
//!
//! The arm64 code of a device build is the code a simulator build would have (both are arm64; x86Sim translates either
//! way), so the conversion is metadata:
//!   - every Mach-O in the bundle (the app, Frameworks/, PlugIns/ and Extensions/ appexes, a Watch/ app, nested
//!     bundles): LC_BUILD_VERSION's platform becomes the matching simulator one (iOS 2 -> 7, tvOS 3 -> 8, watchOS 4 -> 9,
//!     visionOS 11 -> 12). An old binary's LC_VERSION_MIN_* (which has no simulator variant on arm64) is rewritten as
//!     an LC_BUILD_VERSION in the header's padding. A fat file keeps only its arm64 slice (armv7 / arm64e / x86_64
//!     slices go; the translator runs plain arm64 only).
//!   - FairPlay: a binary whose LC_ENCRYPTION_INFO has cryptid != 0 (App Store downloads) is encrypted for one device
//!     and cannot run anywhere else: refused with a message naming it.
//!   - Info.plists of every bundle: CFBundleSupportedPlatforms / DTPlatformName / DTSDKName name the simulator.
//!   - signature: each binary's entitlements are read from its signature (the entitlements slot) before the change and
//!     put back in an ad-hoc signature (rcodesign, the tool build/linux-bin/codesign wraps). Darling's csops serves the
//!     signature's entitlements when a binary has no __TEXT,__entitlements section (a device build never has one).
//!     The provisioning profile is left in the bundle (inert in the simulator).
//!
//! The input is never changed: an .app is copied (reflink where the file system can) to a work dir, and that copy is
//! converted. A simulator .app passes through untouched.
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const LC_SEGMENT_64: u32 = 0x19;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const LC_ENCRYPTION_INFO: u32 = 0x21;
const LC_ENCRYPTION_INFO_64: u32 = 0x2c;
const LC_VERSION_MIN_IPHONEOS: u32 = 0x25;
const LC_VERSION_MIN_TVOS: u32 = 0x2f;
const LC_VERSION_MIN_WATCHOS: u32 = 0x30;
const LC_BUILD_VERSION: u32 = 0x32;

/// (device platform, its simulator platform, CFBundleSupportedPlatforms device / simulator name, DTPlatformName)
const PLATFORMS: [(u32, u32, &str, &str, &str); 4] = [
    (2, 7, "iPhoneOS", "iPhoneSimulator", "iphonesimulator"),
    (3, 8, "AppleTVOS", "AppleTVSimulator", "appletvsimulator"),
    (4, 9, "WatchOS", "WatchSimulator", "watchsimulator"),
    (11, 12, "XROS", "XRSimulator", "xrsimulator"),
];

pub fn platform_label(p: u32) -> &'static str {
    match p {
        1 => "macOS",
        2 => "iOS",
        3 => "tvOS",
        4 => "watchOS",
        6 => "Mac Catalyst",
        7 => "iOS Simulator",
        8 => "tvOS Simulator",
        9 => "watchOS Simulator",
        11 => "visionOS",
        12 => "visionOS Simulator",
        _ => "unknown platform",
    }
}

fn simulator_of(p: u32) -> Option<u32> {
    PLATFORMS.iter().find(|x| x.0 == p).map(|x| x.1)
}

fn is_simulator(p: u32) -> bool {
    PLATFORMS.iter().any(|x| x.1 == p)
}

// Every read of a file's bytes is bounds-checked: a malformed Mach-O is an error, never a panic (an .ipa is untrusted
// input, and a panic in the MCP server would end it with every pool lease it holds).
fn bytes_at<const N: usize>(b: &[u8], o: usize) -> Result<[u8; N]> {
    o.checked_add(N).and_then(|e| b.get(o..e)).map(|s| s.try_into().unwrap()).ok_or_else(|| anyhow::anyhow!("truncated Mach-O (read at {o:#x} past the end, {:#x})", b.len()))
}
fn rd32(b: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes_at(b, o)?))
}
fn rd64(b: &[u8], o: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes_at(b, o)?))
}
fn rd32be(b: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(bytes_at(b, o)?))
}
fn rd64be(b: &[u8], o: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(bytes_at(b, o)?))
}
fn wr32(b: &mut [u8], o: usize, v: u32) -> Result<()> {
    let n = b.len();
    o.checked_add(4).and_then(|e| b.get_mut(o..e)).ok_or_else(|| anyhow::anyhow!("write at {o:#x} past the end ({n:#x})"))?.copy_from_slice(&v.to_le_bytes());
    Ok(())
}

/// What one Mach-O file is and what the conversion did to it.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Binary {
    /// path inside the bundle
    pub path: String,
    /// platform before (LC_BUILD_VERSION / LC_VERSION_MIN_*), 0 = none recorded
    pub platform: u32,
    pub platform_name: String,
    /// FairPlay: cryptid != 0
    pub encrypted: bool,
    /// slices of a fat file other than arm64 that were dropped (armv7, arm64e, x86_64)
    pub dropped_slices: Vec<String>,
    /// has entitlements in its signature (kept)
    pub entitlements: bool,
    pub changed: bool,
}

fn cpu_name(cpu: u32, sub: u32) -> String {
    match (cpu, sub & 0xff) {
        (CPU_TYPE_ARM64, 2) => "arm64e".into(),
        (CPU_TYPE_ARM64, _) => "arm64".into(),
        (0x0200_000c, _) => "arm64_32".into(),
        (12, _) => "armv7".into(),
        (0x0100_0007, _) => "x86_64".into(),
        (7, _) => "i386".into(),
        _ => format!("cpu {cpu:#x}"),
    }
}

/// Load-command facts of a thin 64-bit image.
#[derive(Debug, Default)]
struct Scan {
    platform: u32,
    encrypted: bool,
    /// the signature's entitlements plist (XML)
    entitlements: Option<Vec<u8>>,
}

/// (ncmds, end of the load commands) of a thin 64-bit image, checked against its length
fn header(b: &[u8]) -> Result<(usize, usize)> {
    anyhow::ensure!(b.len() >= 32 && rd32(b, 0)? == MH_MAGIC_64, "not a 64-bit Mach-O");
    let (ncmds, sizeofcmds) = (rd32(b, 16)? as usize, rd32(b, 20)? as usize);
    let end = 32usize.checked_add(sizeofcmds).filter(|e| *e <= b.len()).ok_or_else(|| anyhow::anyhow!("truncated load commands"))?;
    Ok((ncmds, end))
}

/// The next load command at `off`: (cmd, cmdsize), the whole command inside the load commands
fn load_command(b: &[u8], off: usize, end: usize) -> Result<(u32, usize)> {
    anyhow::ensure!(off.checked_add(8).is_some_and(|e| e <= end), "load commands run past sizeofcmds at {off:#x}");
    let (cmd, cs) = (rd32(b, off)?, rd32(b, off + 4)? as usize);
    anyhow::ensure!(cs >= 8 && off.checked_add(cs).is_some_and(|e| e <= end), "bad load command at {off:#x}");
    let min = match cmd {
        LC_BUILD_VERSION => 24,
        LC_VERSION_MIN_IPHONEOS | LC_VERSION_MIN_TVOS | LC_VERSION_MIN_WATCHOS => 16,
        LC_ENCRYPTION_INFO => 20,
        LC_ENCRYPTION_INFO_64 => 24,
        LC_CODE_SIGNATURE => 16,
        LC_SEGMENT_64 => 72,
        _ => 8,
    };
    anyhow::ensure!(cs >= min, "load command {cmd:#x} at {off:#x} is {cs} bytes, shorter than its {min}");
    if cmd == LC_SEGMENT_64 {
        let nsects = rd32(b, off + 64)? as usize;
        anyhow::ensure!(nsects.checked_mul(80).and_then(|n| n.checked_add(72)).is_some_and(|n| n <= cs), "LC_SEGMENT_64 at {off:#x}: {nsects} sections don't fit its {cs} bytes");
    }
    Ok((cmd, cs))
}

fn scan_thin(b: &[u8]) -> Result<Scan> {
    let (ncmds, end) = header(b)?;
    let mut s = Scan::default();
    let mut off = 32;
    for _ in 0..ncmds {
        let (cmd, cs) = load_command(b, off, end)?;
        match cmd {
            LC_BUILD_VERSION => s.platform = rd32(b, off + 8)?,
            LC_VERSION_MIN_IPHONEOS if s.platform == 0 => s.platform = 2,
            LC_VERSION_MIN_TVOS if s.platform == 0 => s.platform = 3,
            LC_VERSION_MIN_WATCHOS if s.platform == 0 => s.platform = 4,
            LC_ENCRYPTION_INFO | LC_ENCRYPTION_INFO_64 => s.encrypted |= rd32(b, off + 16)? != 0,
            LC_CODE_SIGNATURE => s.entitlements = signature_entitlements(b, rd32(b, off + 8)? as usize, rd32(b, off + 12)? as usize),
            _ => {}
        }
        off += cs;
    }
    Ok(s)
}

/// The entitlements blob (slot 5, magic 0xfade7171) of an embedded signature superblob (0xfade0cc0), big-endian.
fn signature_entitlements(b: &[u8], off: usize, size: usize) -> Option<Vec<u8>> {
    let sig = b.get(off..off.checked_add(size)?)?;
    if rd32be(sig, 0).ok()? != 0xfade_0cc0 {
        return None;
    }
    let count = rd32be(sig, 8).ok()? as usize;
    for i in 0..count.min(sig.len() / 8) {
        let e = 12 + i * 8;
        if rd32be(sig, e).ok()? != 5 {
            continue;
        }
        let o = rd32be(sig, e + 4).ok()? as usize;
        if rd32be(sig, o).ok()? != 0xfade_7171 {
            return None;
        }
        let len = rd32be(sig, o + 4).ok()? as usize;
        let body = sig.get(o.checked_add(8)?..o.checked_add(len)?)?.to_vec();
        // an empty plist (<dict/>) is no entitlements
        let txt = String::from_utf8_lossy(&body);
        return (txt.contains("<key>")).then_some(body);
    }
    None
}

/// Retags a thin 64-bit image in place for the simulator. Returns whether anything changed.
fn retag_thin(b: &mut [u8]) -> Result<bool> {
    let (ncmds, end) = header(b)?;
    let sizeofcmds = end - 32;
    let mut off = 32;
    let mut changed = false;
    let mut vmin: Option<(usize, u32)> = None;
    let mut has_bv = false;
    // the lowest file offset any section's (or non-zero-offset segment's) data starts at: the header padding ends there
    let mut first_data = b.len();
    for _ in 0..ncmds {
        let (cmd, cs) = load_command(b, off, end)?;
        match cmd {
            LC_BUILD_VERSION => {
                has_bv = true;
                let p = rd32(b, off + 8)?;
                if let Some(sim) = simulator_of(p) {
                    wr32(b, off + 8, sim)?;
                    changed = true;
                }
            }
            LC_VERSION_MIN_IPHONEOS | LC_VERSION_MIN_TVOS | LC_VERSION_MIN_WATCHOS => {
                anyhow::ensure!(cs == 16, "LC_VERSION_MIN at {off:#x} is {cs} bytes, not 16");
                let p = match cmd {
                    LC_VERSION_MIN_IPHONEOS => 2,
                    LC_VERSION_MIN_TVOS => 3,
                    _ => 4,
                };
                vmin = Some((off, p));
            }
            LC_SEGMENT_64 => {
                // load_command checked that the nsects sections fit in the command
                let nsects = rd32(b, off + 64)? as usize;
                for i in 0..nsects {
                    let s = off + 72 + i * 80;
                    let (soff, flags) = (rd32(b, s + 48)? as usize, rd32(b, s + 64)?);
                    // zerofill sections (S_ZEROFILL 1, S_GB_ZEROFILL 0xc, S_THREAD_LOCAL_ZEROFILL 0x12) have no file data
                    if soff != 0 && !matches!(flags & 0xff, 0x1 | 0xc | 0x12) {
                        first_data = first_data.min(soff);
                    }
                }
                let fileoff = rd64(b, off + 40)?;
                let filesize = rd64(b, off + 48)?;
                if fileoff != 0 && filesize != 0 {
                    first_data = first_data.min(usize::try_from(fileoff).unwrap_or(usize::MAX));
                }
            }
            _ => {}
        }
        off += cs;
    }
    if let (Some((at, p)), false) = (vmin, has_bv) {
        // LC_VERSION_MIN_* {cmd, size 16, version, sdk} -> LC_BUILD_VERSION {cmd, size 24, platform, minos, sdk, ntools 0}:
        // 8 more bytes, taken from the padding after the load commands
        anyhow::ensure!(end + 8 <= first_data && end + 8 <= b.len(), "no room in the Mach-O header to add LC_BUILD_VERSION (old LC_VERSION_MIN binary linked without header padding)");
        let (ver, sdk) = (rd32(b, at + 8)?, rd32(b, at + 12)?);
        b.copy_within(at + 16..end, at + 24);
        let sim = simulator_of(p).unwrap();
        for (i, v) in [LC_BUILD_VERSION, 24, sim, ver, sdk, 0].into_iter().enumerate() {
            wr32(b, at + i * 4, v)?;
        }
        wr32(b, 20, (sizeofcmds + 8) as u32)?;
        changed = true;
    }
    Ok(changed)
}

fn is_macho(head: &[u8]) -> bool {
    let (Ok(le), Ok(be)) = (rd32(head, 0), rd32be(head, 0)) else { return false };
    le == MH_MAGIC_64 || le == 0xfeed_face || be == FAT_MAGIC || be == FAT_MAGIC_64
}

/// Reads a Mach-O file: the arm64 image (from a fat file: its arm64 slice), dropped slices, and an error for a file
/// without one.
fn arm64_image(bytes: &[u8]) -> Result<(Vec<u8>, Vec<String>, bool)> {
    let be = rd32be(bytes, 0)?;
    if be == FAT_MAGIC || be == FAT_MAGIC_64 {
        let n = rd32be(bytes, 4)? as usize;
        let w = if be == FAT_MAGIC_64 { 32 } else { 20 };
        let mut slices = vec![];
        for i in 0..n {
            let e = 8 + i * w;
            anyhow::ensure!(e + w <= bytes.len(), "truncated fat header");
            let (cpu, sub) = (rd32be(bytes, e)?, rd32be(bytes, e + 4)?);
            let (o, s) = if w == 32 { (rd64be(bytes, e + 8)?, rd64be(bytes, e + 16)?) } else { (rd32be(bytes, e + 8)? as u64, rd32be(bytes, e + 12)? as u64) };
            slices.push((cpu, sub, o, s));
        }
        let pick = slices.iter().find(|s| s.0 == CPU_TYPE_ARM64 && s.1 & 0xff != 2);
        let names: Vec<String> = slices.iter().map(|s| cpu_name(s.0, s.1)).collect();
        let Some(&(_, _, o, s)) = pick else { bail!("no arm64 slice (has {})", names.join(", ")) };
        let slice = usize::try_from(o).ok().zip(usize::try_from(s).ok()).and_then(|(o, s)| bytes.get(o..o.checked_add(s)?)).ok_or_else(|| anyhow::anyhow!("truncated fat slice"))?;
        let dropped = names.into_iter().filter(|n| n != "arm64").collect();
        return Ok((slice.to_vec(), dropped, true));
    }
    anyhow::ensure!(bytes.len() >= 12, "truncated Mach-O header");
    if rd32(bytes, 0)? == 0xfeed_face {
        bail!("32-bit Mach-O ({}): only arm64 runs", cpu_name(rd32(bytes, 4)?, rd32(bytes, 8)?));
    }
    let (cpu, sub) = (rd32(bytes, 4)?, rd32(bytes, 8)?);
    if cpu != CPU_TYPE_ARM64 || sub & 0xff == 2 {
        bail!("built for {}: only arm64 runs", cpu_name(cpu, sub));
    }
    Ok((bytes.to_vec(), vec![], false))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(dir).with_context(|| dir.display().to_string())? {
        let e = e?;
        let t = e.file_type()?;
        if t.is_dir() {
            walk(&e.path(), out)?;
        } else if t.is_file() {
            out.push(e.path());
        }
    }
    Ok(())
}

fn machos(app: &Path) -> Result<Vec<PathBuf>> {
    let mut files = vec![];
    walk(app, &mut files)?;
    files.sort();
    Ok(files
        .into_iter()
        .filter(|p| {
            let mut h = [0u8; 4];
            std::fs::File::open(p).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut h)).is_ok() && is_macho(&h)
        })
        .collect())
}

/// The result of `prepare`.
#[derive(Debug, Clone, Serialize)]
pub struct Prepared {
    /// the .app to install (the input itself for a simulator .app)
    pub app: PathBuf,
    /// what the input was: "simulator app", "device app", "ipa", "xcarchive"
    pub kind: String,
    /// a device build was turned into a simulator one
    pub converted: bool,
    pub binaries: Vec<Binary>,
    pub notes: Vec<String>,
}

/// Is `p` something `prepare` takes (an .app / .ipa / .xcarchive)?
pub fn accepts(p: &Path) -> bool {
    let e = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(e.as_str(), "app" | "ipa" | "xcarchive" | "zip")
}

/// Work dir for conversions: $X86SIM_SCRATCH/devapp/<n> (never /tmp: it is RAM on this host).
pub fn work_dir() -> PathBuf {
    crate::aot::scratch().join("devapp")
}

/// The single .app in `dir` (Payload/ of an ipa, Products/Applications/ of an archive). A symlinked .app is refused:
/// converting "it" would rewrite and re-sign whatever it points at.
fn only_app(dir: &Path, what: &str) -> Result<PathBuf> {
    let apps: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("{what}: no {}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .collect();
    match apps.len() {
        1 => {
            let m = apps[0].symlink_metadata()?;
            anyhow::ensure!(!m.file_type().is_symlink(), "{what}: {} is a symbolic link, not an app bundle", apps[0].display());
            anyhow::ensure!(m.is_dir(), "{what}: {} is not a directory", apps[0].display());
            Ok(apps[0].clone())
        }
        0 => bail!("{what}: no .app in {}", dir.display()),
        _ => bail!("{what}: more than one .app in {}", dir.display()),
    }
}

/// Limits for an unpacked .ipa (a zip bomb would otherwise fill the scratch disk): the biggest App Store apps are a
/// few GB unpacked.
pub const MAX_UNPACKED_BYTES: u64 = 16 << 30;
pub const MAX_UNPACKED_FILES: u64 = 200_000;

/// Refuses an archive entry name that would land outside the extraction dir (unzip strips these, but we don't rely on
/// it): absolute, or with a ".." component.
fn safe_entry_name(n: &str) -> bool {
    !n.starts_with('/') && !n.starts_with('\\') && !n.split(['/', '\\']).any(|c| c == "..") && !n.contains('\0')
}

/// Every symlink under `root` must resolve inside `root` (a link to /home/... or ../../ would let the conversion write
/// through it). Dangling links are refused too: their target could be created later.
pub fn check_contained(root: &Path) -> Result<()> {
    let base = root.canonicalize().with_context(|| root.display().to_string())?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).with_context(|| d.display().to_string())? {
            let e = e?;
            let (p, t) = (e.path(), e.file_type()?);
            if t.is_symlink() {
                let to = std::fs::read_link(&p)?;
                let ok = !to.is_absolute() && p.canonicalize().is_ok_and(|r| r.starts_with(&base));
                anyhow::ensure!(ok, "{}: a symbolic link to {} outside the bundle", p.strip_prefix(root).unwrap_or(&p).display(), to.display());
            } else if t.is_dir() {
                stack.push(p);
            } else {
                anyhow::ensure!(t.is_file(), "{}: not a regular file, directory or symlink", p.strip_prefix(root).unwrap_or(&p).display());
            }
        }
    }
    Ok(())
}

/// Size and file count of a tree (symlinks not followed)
fn tree_size(root: &Path) -> (u64, u64) {
    let (mut bytes, mut files) = (0, 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            files += 1;
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                _ => bytes += e.metadata().map(|m| m.len()).unwrap_or(0),
            }
        }
    }
    (bytes, files)
}

/// Unpacks an .ipa into `un` safely: entry names checked first, the declared size and count capped, unzip watched (a
/// lying header can't get past MAX_UNPACKED_BYTES on disk either), then every symlink checked to stay inside.
fn unpack(ipa: &Path, un: &Path, max_bytes: u64, max_files: u64) -> Result<()> {
    let out = Command::new("unzip").arg("-Z1").arg(ipa).output().context("running unzip")?;
    anyhow::ensure!(out.status.success(), "unzip -Z1: {}", String::from_utf8_lossy(&out.stderr).trim());
    let names = String::from_utf8_lossy(&out.stdout);
    let mut n = 0u64;
    for name in names.lines() {
        anyhow::ensure!(safe_entry_name(name), "the archive has an entry outside its own folder: {name:?}");
        n += 1;
    }
    anyhow::ensure!(n <= max_files, "the archive has {n} entries (more than {max_files})");
    // "N files, X bytes uncompressed, Y bytes compressed: Z%"
    let out = Command::new("unzip").arg("-Zt").arg(ipa).output().context("running unzip")?;
    let t = String::from_utf8_lossy(&out.stdout);
    let declared: u64 = t.split(',').nth(1).and_then(|x| x.split_whitespace().next()).and_then(|x| x.parse().ok()).unwrap_or(0);
    anyhow::ensure!(declared <= max_bytes, "the archive unpacks to {declared} bytes (more than {max_bytes})");
    let mut child = Command::new("unzip").args(["-q", "-o"]).arg(ipa).arg("-d").arg(un).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().context("running unzip")?;
    let mut last = std::time::Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        if last.elapsed() >= std::time::Duration::from_secs(1) {
            last = std::time::Instant::now();
            let (b, f) = tree_size(un);
            if b > max_bytes || f > max_files {
                let _ = child.kill();
                let _ = child.wait();
                bail!("the archive unpacks to more than {max_bytes} bytes or {max_files} files: stopped");
            }
        }
    };
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut e, &mut err);
    }
    anyhow::ensure!(status.success(), "unzip: {}", err.trim());
    let (b, f) = tree_size(un);
    anyhow::ensure!(b <= max_bytes && f <= max_files, "the archive unpacks to more than {max_bytes} bytes or {max_files} files");
    check_contained(un)
}

/// Turns `input` (.app, .ipa, .xcarchive) into a simulator .app. `work`: a fresh dir for the unpacked / converted copy
/// (made; its old contents removed).
pub fn prepare(input: &Path, work: &Path) -> Result<Prepared> {
    prepare_opts(input, work, true)
}

/// What `prepare` would find, without converting anything (an .ipa is still unpacked into `work` to look inside)
pub fn inspect(input: &Path, work: &Path) -> Result<Prepared> {
    prepare_opts(input, work, false)
}

fn prepare_opts(input: &Path, work: &Path, convert: bool) -> Result<Prepared> {
    let input = input.canonicalize().with_context(|| input.display().to_string())?;
    let ext = input.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut notes = vec![];
    let fresh = |d: &Path| -> Result<()> {
        if d.exists() {
            std::fs::remove_dir_all(d)?;
        }
        std::fs::create_dir_all(d)?;
        Ok(())
    };
    let (src, mut kind, unpacked) = if input.is_dir() && ext == "app" {
        (input.clone(), "app", false)
    } else if input.is_dir() && ext == "xcarchive" {
        (only_app(&input.join("Products/Applications"), "xcarchive")?, "xcarchive", false)
    } else if input.is_file() {
        let mut h = [0u8; 4];
        std::fs::File::open(&input).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut h)).context("reading the archive")?;
        anyhow::ensure!(&h == b"PK\x03\x04", "{} is not an .ipa (zip) archive", input.display());
        fresh(work)?;
        let un = work.join("unpacked");
        std::fs::create_dir_all(&un)?;
        // X86SIM_DEVAPP_MAX_MB: a lower cap (tests/devapp/hostile.sh); never a higher one
        let max = std::env::var("X86SIM_DEVAPP_MAX_MB").ok().and_then(|v| v.parse::<u64>().ok()).map(|m| (m << 20).min(MAX_UNPACKED_BYTES)).unwrap_or(MAX_UNPACKED_BYTES);
        unpack(&input, &un, max, MAX_UNPACKED_FILES)?;
        (only_app(&un.join("Payload"), "ipa")?, "ipa", true)
    } else {
        bail!("{}: give an .app, .ipa or .xcarchive", input.display());
    };
    anyhow::ensure!(src.join("Info.plist").is_file(), "{} is not an app bundle (no Info.plist)", src.display());

    // look first: what platform, encryption, slices
    let files = machos(&src)?;
    let exe = info_value(&src, "CFBundleExecutable").unwrap_or_default();
    anyhow::ensure!(!exe.is_empty() && files.contains(&src.join(&exe)), "{}: its executable {exe:?} (CFBundleExecutable) is missing or not a Mach-O", src.display());
    anyhow::ensure!(!files.is_empty(), "{}: no executable in the bundle", src.display());
    let mut bins = vec![];
    let mut problems = vec![];
    for f in &files {
        let rel = f.strip_prefix(&src).unwrap_or(f).to_string_lossy().into_owned();
        let bytes = std::fs::read(f)?;
        let (img, dropped, _) = match arm64_image(&bytes) {
            Ok(x) => x,
            Err(e) => {
                problems.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let s = scan_thin(&img).with_context(|| rel.clone())?;
        bins.push(Binary {
            path: rel,
            platform: s.platform,
            platform_name: platform_label(s.platform).into(),
            encrypted: s.encrypted,
            dropped_slices: dropped,
            entitlements: s.entitlements.is_some(),
            changed: false,
        });
    }
    let enc: Vec<&str> = bins.iter().filter(|b| b.encrypted).map(|b| b.path.as_str()).collect();
    if !enc.is_empty() {
        bail!(
            "{} is encrypted with FairPlay (an App Store download: LC_ENCRYPTION_INFO cryptid=1 in {}). It only runs on the device it was bought for; install a development / ad-hoc / enterprise export (Xcode: Product > Archive > Distribute App > Debugging or Release Testing) or the simulator build",
            input.file_name().unwrap_or_default().to_string_lossy(),
            enc.join(", ")
        );
    }
    anyhow::ensure!(problems.is_empty(), "can't run in the simulator: {}", problems.join("; "));
    let other: Vec<String> = bins.iter().filter(|b| !matches!(b.platform, 0 | 2 | 3 | 4 | 11) && !is_simulator(b.platform)).map(|b| format!("{} ({})", b.path, b.platform_name)).collect();
    anyhow::ensure!(other.is_empty(), "built for another platform: {}", other.join(", "));
    let needs = bins.iter().any(|b| simulator_of(b.platform).is_some() || !b.dropped_slices.is_empty());
    if !needs {
        kind = if kind == "app" { "simulator app" } else { kind };
        return Ok(Prepared { app: src, kind: kind.into(), converted: false, binaries: bins, notes });
    }
    if kind == "app" {
        kind = "device app";
    }
    if !convert {
        notes.push("a device build: install converts it for the simulator".into());
        return Ok(Prepared { app: src, kind: kind.into(), converted: false, binaries: bins, notes });
    }

    // convert a copy
    let dst = if unpacked {
        src.clone()
    } else {
        fresh(work)?;
        let dst = work.join(src.file_name().unwrap());
        let out = Command::new("cp").args(["-a", "--reflink=auto"]).arg(&src).arg(&dst).output().context("copying the app")?;
        anyhow::ensure!(out.status.success(), "cp: {}", String::from_utf8_lossy(&out.stderr).trim());
        dst
    };
    // the copy (or the unpacked .ipa, checked already) is written to below: no link in it may lead out of it
    check_contained(&dst)?;
    let ents_dir = work.join("entitlements");
    std::fs::create_dir_all(&ents_dir)?;
    let mut scoped: Vec<(String, PathBuf)> = vec![];
    for b in bins.iter_mut() {
        let p = dst.join(&b.path);
        let bytes = std::fs::read(&p)?;
        let (mut img, _, thinned) = arm64_image(&bytes)?;
        let ents = scan_thin(&img)?.entitlements;
        let changed = retag_thin(&mut img).with_context(|| b.path.clone())?;
        if changed || thinned {
            std::fs::write(&p, &img)?;
            b.changed = true;
        }
        if let Some(e) = ents {
            let f = ents_dir.join(format!("{}.plist", scoped.len()));
            std::fs::write(&f, e)?;
            scoped.push((b.path.clone(), f));
        }
    }
    let dropped: Vec<String> = bins.iter().filter(|b| !b.dropped_slices.is_empty()).map(|b| format!("{} ({})", b.path, b.dropped_slices.join(", "))).collect();
    if !dropped.is_empty() {
        notes.push(format!("kept the arm64 slice only: {}", dropped.join("; ")));
    }
    let plists = retag_plists(&dst)?;
    notes.push(format!("Info.plist platform keys set to the simulator in {plists} bundle(s)"));
    sign(&dst, &scoped)?;
    notes.push(format!("ad-hoc signed; entitlements kept for {} binar{}", scoped.len(), if scoped.len() == 1 { "y" } else { "ies" }));
    if dst.join("embedded.mobileprovision").exists() {
        notes.push("embedded.mobileprovision left in place (the simulator does not read it)".into());
    }
    Ok(Prepared { app: dst, kind: kind.into(), converted: true, binaries: bins, notes })
}

/// Rewrites the platform keys of every bundle's Info.plist (XML or binary, kept in its format). Returns how many.
fn retag_plists(app: &Path) -> Result<usize> {
    let map: Vec<String> = PLATFORMS.iter().map(|p| format!("{}:{}:{}", p.2, p.3, p.4)).collect();
    // plistlib (the repo's scripts rely on python3); -I: nothing from the bundle's directories is imported
    const PY: &str = r#"
import os, plistlib, sys
app = sys.argv[1]
m = {a: (b, c) for a, b, c in (x.split(':') for x in sys.argv[2:])}
n = 0
for d, _, fs in os.walk(app):
    if 'Info.plist' not in fs or (d != app and not os.path.splitext(d)[1]):
        continue
    p = os.path.join(d, 'Info.plist')
    if os.path.islink(p):
        continue
    raw = open(p, 'rb').read()
    try:
        pl = plistlib.loads(raw)
    except Exception:
        continue
    if not isinstance(pl, dict):
        continue
    sp = pl.get('CFBundleSupportedPlatforms')
    dev = next((x for x in (sp or []) if x in m), None)
    if dev is None and pl.get('DTPlatformName') in [k.lower() for k in m]:
        dev = next(k for k in m if k.lower() == pl['DTPlatformName'])
    if dev is None:
        continue
    sim, dt = m[dev]
    pl['CFBundleSupportedPlatforms'] = [sim]
    pl['DTPlatformName'] = dt
    if isinstance(pl.get('DTSDKName'), str) and pl['DTSDKName'].lower().startswith(dev.lower()):
        pl['DTSDKName'] = dt + pl['DTSDKName'][len(dev):]
    fmt = plistlib.FMT_BINARY if raw.startswith(b'bplist') else plistlib.FMT_XML
    open(p, 'wb').write(plistlib.dumps(pl, fmt=fmt))
    n += 1
print(n)
"#;
    let out = Command::new("python3").args(["-I", "-c", PY]).arg(app).args(&map).output().context("python3 (Info.plist)")?;
    anyhow::ensure!(out.status.success(), "Info.plist: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0))
}

/// rcodesign, as build/linux-bin/codesign finds it (PATH, then ~/.cargo/bin)
fn rcodesign() -> Result<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(h) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(h).join(".cargo/bin"));
    }
    dirs.into_iter().map(|d| d.join("rcodesign")).find(|p| p.is_file()).ok_or_else(|| anyhow::anyhow!("rcodesign not found (cargo install apple-codesign): needed to re-sign the converted app"))
}

/// Ad-hoc signs the bundle (nested bundles too, inside out, as rcodesign does) with each binary's own entitlements:
/// the main executable's unscoped, the others scoped by their bundle-relative path.
fn sign(app: &Path, ents: &[(String, PathBuf)]) -> Result<()> {
    let main_exe = info_value(app, "CFBundleExecutable").unwrap_or_default();
    let mut cmd = Command::new(rcodesign()?);
    cmd.arg("sign");
    for (rel, f) in ents {
        // rcodesign's scoped form is PATH:FILE: a ':' in the bundle path would be read as the separator
        anyhow::ensure!(*rel == main_exe || !rel.contains(':'), "{rel}: a ':' in a binary's path can't be passed to rcodesign");
        if *rel == main_exe {
            cmd.arg("--entitlements-xml-file").arg(f);
        } else {
            cmd.arg("--entitlements-xml-file").arg(format!("{rel}:{}", f.display()));
        }
    }
    let out = cmd.arg(app).output().context("running rcodesign")?;
    anyhow::ensure!(out.status.success(), "rcodesign sign: {}", String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.contains("error") || l.contains("Error")).collect::<Vec<_>>().join("; "));
    Ok(())
}

fn info_value(app: &Path, key: &str) -> Option<String> {
    let out = Command::new("python3")
        .args(["-I", "-c", "import plistlib,sys;print(plistlib.load(open(sys.argv[1],'rb')).get(sys.argv[2],''))"])
        .arg(app.join("Info.plist"))
        .arg(key)
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Entitlements in a file's signature (for checks and tests): the XML, or None.
pub fn entitlements_of(file: &Path) -> Result<Option<String>> {
    let (img, _, _) = arm64_image(&std::fs::read(file)?)?;
    Ok(scan_thin(&img)?.entitlements.map(|v| String::from_utf8_lossy(&v).into_owned()))
}

/// Platform of a file's arm64 image (for checks and tests)
pub fn platform_of(file: &Path) -> Result<u32> {
    let (img, _, _) = arm64_image(&std::fs::read(file)?)?;
    Ok(scan_thin(&img)?.platform)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macho(cmds: &[Vec<u8>], pad: usize) -> Vec<u8> {
        let size: usize = cmds.iter().map(|c| c.len()).sum();
        let mut b = vec![];
        for v in [MH_MAGIC_64, CPU_TYPE_ARM64, 0, 2, cmds.len() as u32, size as u32, 0, 0] {
            b.extend(v.to_le_bytes());
        }
        for c in cmds {
            b.extend(c);
        }
        b.resize(b.len() + pad, 0);
        b
    }
    fn cmd(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn build_version_is_retagged() {
        let mut b = macho(&[cmd(&[LC_BUILD_VERSION, 24, 2, 0x1b0000, 0x1b0200, 0])], 64);
        assert!(retag_thin(&mut b).unwrap());
        assert_eq!(scan_thin(&b).unwrap().platform, 7);
        let mut tv = macho(&[cmd(&[LC_BUILD_VERSION, 24, 3, 0, 0, 0])], 0);
        retag_thin(&mut tv).unwrap();
        assert_eq!(scan_thin(&tv).unwrap().platform, 8);
    }

    #[test]
    fn version_min_becomes_build_version() {
        let mut b = macho(&[cmd(&[LC_VERSION_MIN_IPHONEOS, 16, 0x0c0000, 0x0e0000]), cmd(&[0x1b, 24, 1, 2, 3, 4])], 16);
        assert!(retag_thin(&mut b).unwrap());
        assert_eq!(rd32(&b, 20).unwrap(), 48);
        assert_eq!(&b[32..56], &cmd(&[LC_BUILD_VERSION, 24, 7, 0x0c0000, 0x0e0000, 0])[..]);
        // the following command moved along intact
        assert_eq!(&b[56..80], &cmd(&[0x1b, 24, 1, 2, 3, 4])[..]);
        // without padding it is refused
        let mut tight = macho(&[cmd(&[LC_VERSION_MIN_IPHONEOS, 16, 0, 0])], 0);
        assert!(retag_thin(&mut tight).is_err());
    }

    #[test]
    fn encryption_is_seen() {
        let b = macho(&[cmd(&[LC_ENCRYPTION_INFO_64, 24, 0x4000, 0x1000, 1, 0])], 0);
        assert!(scan_thin(&b).unwrap().encrypted);
        let b = macho(&[cmd(&[LC_ENCRYPTION_INFO_64, 24, 0x4000, 0x1000, 0, 0])], 0);
        assert!(!scan_thin(&b).unwrap().encrypted);
    }

    #[test]
    fn fat_keeps_arm64() {
        let thin = macho(&[cmd(&[LC_BUILD_VERSION, 24, 2, 0, 0, 0])], 0);
        let mut fat = vec![];
        for v in [FAT_MAGIC, 2, 12, 9, 64, 0, 14, CPU_TYPE_ARM64, 0, 64 + thin.len() as u32, thin.len() as u32, 14] {
            fat.extend(v.to_be_bytes());
        }
        fat.resize(64, 0);
        fat.extend(&thin); // "armv7" slice (content irrelevant)
        fat.extend(&thin);
        let (img, dropped, thinned) = arm64_image(&fat).unwrap();
        assert!(thinned);
        assert_eq!(dropped, vec!["armv7".to_string()]);
        assert_eq!(img, thin);
        // arm64e alone is refused
        let mut e = thin.clone();
        wr32(&mut e, 8, 2).unwrap();
        assert!(arm64_image(&e).is_err());
    }

    #[test]
    fn entitlements_from_signature() {
        let xml = b"<?xml version=\"1.0\"?><plist><dict><key>application-identifier</key><string>T.x</string></dict></plist>";
        let mut blob = vec![];
        blob.extend(0xfade_7171u32.to_be_bytes());
        blob.extend(((8 + xml.len()) as u32).to_be_bytes());
        blob.extend(xml);
        let mut sig = vec![];
        sig.extend(0xfade_0cc0u32.to_be_bytes());
        sig.extend(((20 + blob.len()) as u32).to_be_bytes());
        sig.extend(1u32.to_be_bytes());
        sig.extend(5u32.to_be_bytes());
        sig.extend(20u32.to_be_bytes());
        sig.extend(&blob);
        let mut b = macho(&[cmd(&[LC_CODE_SIGNATURE, 16, 0, sig.len() as u32])], 0);
        let at = b.len() as u32;
        wr32(&mut b, 32 + 8, at).unwrap();
        b.extend(&sig);
        assert_eq!(scan_thin(&b).unwrap().entitlements.as_deref(), Some(&xml[..]));
    }

    #[test]
    fn malformed_machos_are_errors_not_panics() {
        // the review's case: ncmds 2, sizeofcmds 24 (one LC_BUILD_VERSION), the file ends there
        let mut b = macho(&[cmd(&[LC_BUILD_VERSION, 24, 2, 0, 0, 0])], 0);
        wr32(&mut b, 16, 2).unwrap();
        assert!(scan_thin(&b).is_err());
        assert!(retag_thin(&mut b.clone()).is_err());
        // short commands: LC_BUILD_VERSION of 8 bytes, LC_SEGMENT_64 of 8, sections past the command, VERSION_MIN of 24
        for c in [cmd(&[LC_BUILD_VERSION, 8]), cmd(&[LC_SEGMENT_64, 8]), cmd(&[LC_ENCRYPTION_INFO_64, 8]), cmd(&[LC_CODE_SIGNATURE, 8])] {
            let mut b = macho(&[c], 0);
            assert!(scan_thin(&b).is_err());
            assert!(retag_thin(&mut b).is_err());
        }
        let mut seg = vec![0u32; 18];
        seg[0] = LC_SEGMENT_64;
        seg[1] = 72;
        seg[16] = 0xffff_ffff; // nsects
        let mut b = macho(&[cmd(&seg)], 0);
        assert!(retag_thin(&mut b).is_err());
        let mut b = macho(&[cmd(&[LC_VERSION_MIN_IPHONEOS, 24, 0, 0, 0, 0])], 64);
        assert!(retag_thin(&mut b).is_err());
        // a cmdsize that wraps, sizeofcmds past the end
        let mut b = macho(&[cmd(&[LC_BUILD_VERSION, 24, 2, 0, 0, 0])], 0);
        wr32(&mut b, 36, u32::MAX).unwrap();
        assert!(scan_thin(&b).is_err());
        let mut b = macho(&[], 0);
        wr32(&mut b, 20, u32::MAX).unwrap();
        assert!(scan_thin(&b).is_err());
        // arm64_image: 4 bytes of a thin magic, a fat slice whose offset + size wraps, a huge fat count
        assert!(arm64_image(&MH_MAGIC_64.to_le_bytes()).is_err());
        assert!(arm64_image(&[]).is_err());
        let mut fat = vec![];
        for v in [FAT_MAGIC_64, 1, CPU_TYPE_ARM64, 0] {
            fat.extend(v.to_be_bytes());
        }
        fat.extend(u64::MAX.to_be_bytes());
        fat.extend(16u64.to_be_bytes());
        fat.extend(0u64.to_be_bytes());
        assert!(arm64_image(&fat).is_err());
        let mut fat = vec![];
        for v in [FAT_MAGIC, u32::MAX] {
            fat.extend(v.to_be_bytes());
        }
        assert!(arm64_image(&fat).is_err());
        // a signature with a huge count and a blob length below its header
        let mut sig = vec![];
        for v in [0xfade_0cc0u32, 0, u32::MAX, 5, 20, 0xfade_7171, 2] {
            sig.extend(v.to_be_bytes());
        }
        assert!(signature_entitlements(&sig, 0, sig.len()).is_none());
        assert!(signature_entitlements(&sig, usize::MAX, 2).is_none());
    }

    /// A fuzz-ish pass: every truncation and many single-byte corruptions of valid images never panic
    #[test]
    fn truncations_and_corruptions_never_panic() {
        let seg = {
            let mut w = vec![0u32; 18 + 20];
            w[0] = LC_SEGMENT_64;
            w[1] = 152;
            w[16] = 1;
            w[18 + 12] = 0x400;
            w
        };
        let base = macho(&[cmd(&[LC_VERSION_MIN_IPHONEOS, 16, 0x0c0000, 0x0e0000]), cmd(&seg), cmd(&[LC_ENCRYPTION_INFO_64, 24, 0, 0, 0, 0])], 64);
        for n in 0..base.len() {
            let _ = scan_thin(&base[..n]);
            let _ = retag_thin(&mut base[..n].to_vec());
            let _ = arm64_image(&base[..n]);
        }
        let mut x: u32 = 0x1234_5678;
        for _ in 0..20000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let mut b = base.clone();
            let i = x as usize % b.len();
            b[i] = (x >> 8) as u8;
            let _ = scan_thin(&b);
            let _ = retag_thin(&mut b);
        }
    }

    #[test]
    fn entry_names_and_links_stay_inside() {
        assert!(safe_entry_name("Payload/A.app/Info.plist"));
        assert!(!safe_entry_name("/etc/passwd"));
        assert!(!safe_entry_name("Payload/../../x"));
        assert!(!safe_entry_name("..\\x"));
        let d = crate::aot::scratch().join(format!("devapp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let root = d.join("root");
        std::fs::create_dir_all(root.join("A.app/Frameworks")).unwrap();
        std::fs::write(root.join("A.app/Info.plist"), b"x").unwrap();
        std::os::unix::fs::symlink("../Info.plist", root.join("A.app/Frameworks/ok")).unwrap();
        check_contained(&root).unwrap();
        std::os::unix::fs::symlink("../../../outside", root.join("A.app/bad")).unwrap();
        assert!(check_contained(&root).is_err());
        std::fs::remove_file(root.join("A.app/bad")).unwrap();
        std::os::unix::fs::symlink(&d, root.join("A.app/abs")).unwrap();
        assert!(check_contained(&root).is_err());
        std::fs::remove_file(root.join("A.app/abs")).unwrap();
        // a symlinked .app is refused by only_app
        std::fs::create_dir_all(d.join("Payload")).unwrap();
        std::os::unix::fs::symlink(root.join("A.app"), d.join("Payload/Evil.app")).unwrap();
        assert!(only_app(&d.join("Payload"), "ipa").unwrap_err().to_string().contains("symbolic link"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
