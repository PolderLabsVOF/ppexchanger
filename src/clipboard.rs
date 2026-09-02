//! Clipboard-image fetch helper used by the `/paste-image` slash
//! command.
//!
//! The fetch is intentionally cross-platform via small CLI tools
//! rather than per-platform Rust crates:
//!
//! * Wayland: `wl-paste --type image` (reads the Wayland clipboard).
//! * X11: `xclip -selection clipboard -t image/png -o` then
//!   `xsel --clipboard --output` as a fallback.
//! * macOS: `pngpaste` (no MIME flag — it returns PNG by definition).
//!
//! The first tool that succeeds with a non-empty payload wins. MIME
//! is then re-derived from the magic bytes so the peer's
//! `FileOffer` reports `image/png` vs `image/jpeg` truthfully —
//! `wl-paste --type image` returns whatever's on the clipboard, not
//! a normalised PNG.
//!
//! The command spawn is behind a trait (`ClipboardSpawn`) so unit
//! tests can stub it without invoking real subprocesses. The
//! production impl is `RealClipboardSpawn`.

use std::io::{self, Read};
use std::process::{Command, Stdio};

/// One image pulled from the clipboard. `width`/`height` come from
/// parsing the PNG or JPEG header in-process; we don't trust the
/// source tool's claims.
#[derive(Debug, Clone)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// One attempt to fetch an image from a clipboard helper.
/// `Ok(Some(bytes))` = tool ran and produced an image,
/// `Ok(None)` = tool ran successfully but no image was on the
/// clipboard (fall through to the next tool),
/// `Err(_)` = tool wasn't installed or otherwise unavailable
/// (also fall through).
pub type ToolAttempt = io::Result<Option<Vec<u8>>>;

/// Trait abstracting `std::process::Command` so unit tests can
/// inject fake behaviour without spawning real subprocesses.
pub trait ClipboardSpawn {
    fn try_tool(&self, program: &str, args: &[&str]) -> ToolAttempt;
}

/// Production impl that spawns real subprocesses.
pub struct RealClipboardSpawn;

impl ClipboardSpawn for RealClipboardSpawn {
    fn try_tool(&self, program: &str, args: &[&str]) -> ToolAttempt {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            // Binary not installed (ENOENT) is the common case;
            // surface it as `Ok(None)` so the helper loop falls
            // through to the next tool rather than aborting.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut stdout = match child.stdout.take() {
            Some(s) => s,
            None => return Ok(None),
        };
        let mut buf = Vec::new();
        // Drain stdout but don't block forever if the child hangs —
        // `wait` would, so just collect what's already buffered.
        let _ = stdout.read_to_end(&mut buf);
        let status = child.wait()?;
        if status.success() && !buf.is_empty() {
            Ok(Some(buf))
        } else {
            Ok(None)
        }
    }
}

/// Detect a MIME type from the leading magic bytes. Only PNG and
/// JPEG are recognised — anything else is rejected.
fn sniff_mime(bytes: &[u8]) -> io::Result<&'static str> {
    if bytes.len() >= 4 && &bytes[..4] == b"\x89PNG" {
        Ok("image/png")
    } else if bytes.len() >= 3 && &bytes[..3] == b"\xFF\xD8\xFF" {
        Ok("image/jpeg")
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "clipboard payload is not a PNG or JPEG",
        ))
    }
}

/// Read pixel width/height from a PNG or JPEG header. We don't
/// decode the image — just enough of the header to extract
/// dimensions for the wire protocol's `FileOffer`.
fn sniff_dimensions(mime: &str, bytes: &[u8]) -> io::Result<(u32, u32)> {
    if mime == "image/png" {
        sniff_png_dimensions(bytes)
    } else if mime == "image/jpeg" {
        sniff_jpeg_dimensions(bytes)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported mime for dims: {}", mime),
        ))
    }
}

/// IHDR is the first chunk after the 8-byte PNG signature. Width
/// and height live at offsets 16 and 20 from the start.
fn sniff_png_dimensions(bytes: &[u8]) -> io::Result<(u32, u32)> {
    if bytes.len() < 24 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "png payload too short for IHDR",
        ));
    }
    if &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "png magic mismatch",
        ));
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "png width slice")
    })?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "png height slice")
    })?);
    Ok((w, h))
}

/// Walk JPEG markers until we find SOF0/SOF2 which carries width
/// and height. Skips APPn/quantisation/Huffman tables, which is
/// enough for any normal JPEG. Stops at first SOF.
fn sniff_jpeg_dimensions(bytes: &[u8]) -> io::Result<(u32, u32)> {
    if bytes.len() < 4 || &bytes[..3] != b"\xFF\xD8\xFF" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "jpeg magic mismatch",
        ));
    }
    let mut i = 2;
    while i + 1 < bytes.len() {
        if bytes[i] != 0xFF {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("jpeg marker expected at offset {}", i),
            ));
        }
        // Skip fill bytes between markers.
        while i < bytes.len() && bytes[i] == 0xFF {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let marker = bytes[i];
        i += 1;
        // SOF0 / SOF2 carry the dimensions.
        if marker == 0xC0 || marker == 0xC2 {
            if i + 7 > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated jpeg SOF",
                ));
            }
            let h = u16::from_be_bytes(bytes[i + 3..i + 5].try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "jpeg h slice")
            })?) as u32;
            let w = u16::from_be_bytes(bytes[i + 5..i + 7].try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "jpeg w slice")
            })?) as u32;
            return Ok((w, h));
        }
        // Stand-alone markers (no length field).
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if i + 2 > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated jpeg segment",
            ));
        }
        let seg_len = u16::from_be_bytes(bytes[i..i + 2].try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "jpeg seg len")
        })?) as usize;
        if seg_len < 2 || i + seg_len > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "jpeg segment length invalid",
            ));
        }
        i += seg_len;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "no SOF marker found in jpeg",
    ))
}

/// Pull the clipboard image by trying each helper in order. The
/// first one that returns a non-empty payload wins.
pub fn paste_clipboard_image(spawn: &dyn ClipboardSpawn) -> io::Result<ClipboardImage> {
    // Order matters: Wayland first when present, then X11
    // fallbacks, then macOS.
    let attempts: &[(&str, &[&str])] = &[
        ("wl-paste", &["--type", "image"]),
        ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
        ("pngpaste", &[]),
    ];
    let mut last_err: Option<String> = None;
    for (prog, args) in attempts {
        match spawn.try_tool(prog, args) {
            Ok(Some(bytes)) => {
                let mime = sniff_mime(&bytes)?.to_string();
                let (width, height) = sniff_dimensions(&mime, &bytes)?;
                return Ok(ClipboardImage {
                    bytes,
                    mime,
                    width,
                    height,
                });
            }
            Ok(None) => continue,
            Err(e) => {
                last_err = Some(format!("{}: {}", prog, e));
                continue;
            }
        }
    }
    let detail = last_err
        .map(|s| format!(" ({})", s))
        .unwrap_or_default();
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no image on clipboard or no clipboard tool found{}", detail),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// In-memory clipboard stub. Each `try_tool` invocation pops the
/// next scripted outcome in FIFO order so the test setup reads
/// naturally: scripts[0] is returned for the first `try_tool` call,
/// scripts[1] for the second, etc.
    struct StubSpawn {
        scripts: RefCell<std::collections::VecDeque<ToolAttempt>>,
        /// Recorded (program, args) pairs so tests can assert which
        /// tool the loop tried.
        log: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl StubSpawn {
        fn new(scripts: Vec<ToolAttempt>) -> Self {
            Self {
                scripts: RefCell::new(scripts.into_iter().collect()),
                log: RefCell::new(Vec::new()),
            }
        }
    }

    impl ClipboardSpawn for StubSpawn {
        fn try_tool(&self, program: &str, args: &[&str]) -> ToolAttempt {
            self.log.borrow_mut().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            self.scripts
                .borrow_mut()
                .pop_front()
                .unwrap_or(Err(io::Error::new(io::ErrorKind::Other, "no script queued")))
        }
    }

    /// Smallest valid 1×1 transparent PNG.
    fn png_1x1() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR len+type
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // width=1, height=1
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, // depth/color/etc + CRC
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT len+type
            0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05,
            0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00,
            0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42,
            0x60, 0x82,
        ]
    }

    /// Minimal valid JPEG with SOF0 reporting 3×2.
    fn jpeg_3x2() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x02];
        // SOF0: length 0x0011 (17 incl. itself), precision 8, h=2, w=3.
        v.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x02, 0x00, 0x03, 0x01, 0x22,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        v
    }

    #[test]
    fn sniff_png_dimensions_minimal() {
        let (w, h) = sniff_png_dimensions(&png_1x1()).unwrap();
        assert_eq!((w, h), (1, 1));
    }

    #[test]
    fn sniff_mime_recognises_png_and_jpeg() {
        assert_eq!(sniff_mime(&png_1x1()).unwrap(), "image/png");
        assert_eq!(sniff_mime(&jpeg_3x2()).unwrap(), "image/jpeg");
        let (w, h) = sniff_jpeg_dimensions(&jpeg_3x2()).unwrap();
        assert_eq!((w, h), (3, 2));
    }

    #[test]
    fn sniff_mime_rejects_unknown() {
        let bytes = b"not an image at all, just text";
        assert!(sniff_mime(bytes).is_err());
    }

    #[test]
    fn sniff_png_dimensions_rejects_short() {
        assert!(sniff_png_dimensions(&png_1x1()[..16]).is_err());
    }

    #[test]
    fn paste_returns_first_successful_tool() {
        // wl-paste returns PNG, the rest never get queried.
        let stub = StubSpawn::new(vec![
            Ok(None),
            Ok(None),
            Ok(None),
        ]);
        // We've queued three fall-throughs but the first call returns
        // Ok(None) which means we loop to the next tool. So set up a
        // first-tool-wins script instead.
        let stub = StubSpawn::new(vec![
            Ok(Some(png_1x1())),
            Ok(None),
            Ok(None),
            Ok(None),
        ]);
        let img = paste_clipboard_image(&stub).unwrap();
        assert_eq!(img.mime, "image/png");
        assert_eq!((img.width, img.height), (1, 1));
        // Only one tool was tried — the others stay in the queue.
        assert_eq!(stub.log.borrow().len(), 1);
        assert_eq!(stub.log.borrow()[0].0, "wl-paste");
    }

    #[test]
    fn paste_falls_through_to_next_tool() {
        // wl-paste returns no image, xclip returns PNG. The
        // StubSpawn scripts are returned in FIFO order — scripts[0]
        // is what `wl-paste` sees, scripts[1] is what `xclip` sees.
        let stub = StubSpawn::new(vec![
            Ok(None),              // wl-paste: no image
            Ok(Some(png_1x1())),  // xclip: PNG
            Ok(None),
            Ok(None),
        ]);
        let img = paste_clipboard_image(&stub).unwrap();
        assert_eq!(img.mime, "image/png");
        let log = stub.log.borrow();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, "wl-paste");
        assert_eq!(log[1].0, "xclip");
    }

    #[test]
    fn paste_skips_tools_that_return_err() {
        // wl-paste errors out (binary missing?), xclip returns PNG.
        let stub = StubSpawn::new(vec![
            Ok(Some(png_1x1())),
            Err(io::Error::new(io::ErrorKind::NotFound, "no xclip")),
            Err(io::Error::new(io::ErrorKind::NotFound, "no xsel")),
            Err(io::Error::new(io::ErrorKind::NotFound, "no pngpaste")),
        ]);
        let img = paste_clipboard_image(&stub).unwrap();
        assert_eq!(img.mime, "image/png");
    }

    #[test]
    fn paste_reports_not_found_when_all_tools_exhausted() {
        let stub = StubSpawn::new(vec![
            Ok(None),
            Ok(None),
            Ok(None),
            Ok(None),
        ]);
        let err = paste_clipboard_image(&stub).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("no image on clipboard"));
        // Every tool was queried.
        assert_eq!(stub.log.borrow().len(), 4);
    }

    #[test]
    fn paste_reports_error_when_mime_is_garbage() {
        // Tool returned something that isn't a PNG or JPEG.
        let stub = StubSpawn::new(vec![Ok(Some(b"garbage".to_vec()))]);
        let err = paste_clipboard_image(&stub).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn paste_handles_jpeg_payload() {
        let stub = StubSpawn::new(vec![
            Ok(None),
            Ok(None),
            Ok(None),
            Ok(Some(jpeg_3x2())),
        ]);
        let img = paste_clipboard_image(&stub).unwrap();
        assert_eq!(img.mime, "image/jpeg");
        assert_eq!((img.width, img.height), (3, 2));
    }

    #[test]
    fn real_spawn_reports_not_found_for_missing_binary() {
        let spawn = RealClipboardSpawn;
        let res = spawn.try_tool("definitely-not-a-real-binary-ppx-test", &[]);
        // Missing binary → Ok(None) so the helper falls through.
        assert!(matches!(res, Ok(None)));
    }
}