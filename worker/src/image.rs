//! Image checks without decoding pixels (design §5 · plan W1): type from magic bytes (never
//! the request's content-type), a full structural walk (no trailing bytes, no polyglots),
//! exact W×H, size cap, no animation, and EXIF only if harmless: Orientation must be
//! absent or 1 (browsers rotate by it, so the displayed size would differ from the header)
//! and no GPS data (TIFF-EXIF GPS IFD or XMP GPS — would leak location from a public R2
//! object). All length arithmetic is checked: `usize` is 32-bit on wasm32, so this module is
//! also fuzzed as wasm32 (`fuzz-wasm/`). Moderation (W2) sees the same bytes; R2 must serve
//! them with the `ImageKind` content-type + `X-Content-Type-Options: nosniff`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ImageKind {
    Png,
    Jpeg,
    Webp,
}

impl ImageKind {
    pub fn ext(self) -> &'static str {
        match self {
            ImageKind::Png => "png",
            ImageKind::Jpeg => "jpg",
            ImageKind::Webp => "webp",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            ImageKind::Png => "image/png",
            ImageKind::Jpeg => "image/jpeg",
            ImageKind::Webp => "image/webp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ImageError {
    Empty,
    TooLarge {
        max: usize,
        got: usize,
    },
    UnknownType,
    /// Header / structure invalid or cut short.
    Malformed(&'static str),
    Animated,
    /// EXIF rotates the image (Orientation ≠ 1) or the file carries GPS data — strip it.
    Exif,
    ZeroSize,
    WrongSize {
        expected: (u32, u32),
        got: (u32, u32),
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub kind: ImageKind,
    pub width: u32,
    pub height: u32,
}

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const XMP_JPEG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXT_JPEG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const XMP_PNG: &[u8] = b"XML:com.adobe.xmp\0";

/// `b[o..o+n]`, `None` on overflow or out of bounds (32-bit safe).
fn slice(b: &[u8], o: usize, n: usize) -> Option<&[u8]> {
    b.get(o..o.checked_add(n)?)
}
fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(slice(b, o, 4)?.try_into().ok()?))
}
fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(slice(b, o, 2)?.try_into().ok()?))
}
fn le16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(slice(b, o, 2)?.try_into().ok()?))
}
fn le24(b: &[u8], o: usize) -> Option<u32> {
    let s = slice(b, o, 3)?;
    Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
}
fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(slice(b, o, 4)?.try_into().ok()?))
}

/// TIFF-structured EXIF (JPEG APP1 after "Exif\0\0", PNG eXIf, WebP EXIF): read IFD0 only;
/// reject Orientation (0x0112, SHORT or LONG) ≠ 1 and any GPS IFD pointer (0x8825).
/// Unparseable EXIF is rejected (fail closed).
fn check_exif(tiff: &[u8]) -> Result<(), ImageError> {
    let bad = || ImageError::Exif;
    let le = match slice(tiff, 0, 4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err(bad()),
    };
    let u16_at = |o: usize| -> Option<u16> {
        let s: [u8; 2] = slice(tiff, o, 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(s)
        } else {
            u16::from_be_bytes(s)
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let s: [u8; 4] = slice(tiff, o, 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(s)
        } else {
            u32::from_be_bytes(s)
        })
    };
    let ifd = u32_at(4).ok_or_else(bad)? as usize;
    let count = u16_at(ifd).ok_or_else(bad)? as usize;
    for i in 0..count {
        let e = i
            .checked_mul(12)
            .and_then(|x| x.checked_add(2))
            .and_then(|x| x.checked_add(ifd))
            .ok_or_else(bad)?;
        match u16_at(e).ok_or_else(bad)? {
            0x0112 => {
                let v = match u16_at(e + 2).ok_or_else(bad)? {
                    3 => u16_at(e + 8).ok_or_else(bad)? as u32,
                    4 => u32_at(e + 8).ok_or_else(bad)?,
                    _ => return Err(bad()),
                };
                if v != 1 {
                    return Err(bad());
                }
            }
            0x8825 => return Err(bad()),
            _ => {}
        }
    }
    Ok(())
}

/// XMP packet: reject GPS coordinates (Photoshop / Lightroom exports write them).
fn check_xmp(xmp: &[u8]) -> Result<(), ImageError> {
    let has = |needle: &[u8]| xmp.windows(needle.len()).any(|w| w == needle);
    if has(b"GPSLatitude") || has(b"GPSLongitude") || has(b"GPSPosition") || has(b"GPSDest") {
        return Err(ImageError::Exif);
    }
    Ok(())
}

/// PNG: IHDR first (length 13), then chunks up to IEND with ≥ 1 IDAT; IEND must end the
/// file. acTL/fcTL/fdAT (APNG) → Animated · eXIf → `check_exif` · iTXt XMP → `check_xmp`.
fn png(b: &[u8]) -> Result<ImageInfo, ImageError> {
    let m = ImageError::Malformed;
    if be32(b, 8) != Some(13) || slice(b, 12, 4) != Some(b"IHDR") {
        return Err(m("png IHDR"));
    }
    let width = be32(b, 16).ok_or(m("png IHDR"))?;
    let height = be32(b, 20).ok_or(m("png IHDR"))?;
    let mut off = 8usize;
    let mut idat = false;
    loop {
        let len = be32(b, off).ok_or(m("png chunk"))? as usize;
        let kind = slice(b, off + 4, 4).ok_or(m("png chunk"))?;
        let data = slice(b, off + 8, len).ok_or(m("png chunk length"))?;
        let next = off
            .checked_add(12)
            .and_then(|o| o.checked_add(len))
            .filter(|n| *n <= b.len())
            .ok_or(m("png chunk length"))?;
        match kind {
            b"acTL" | b"fcTL" | b"fdAT" => return Err(ImageError::Animated),
            b"IHDR" if off != 8 => return Err(m("png second IHDR")),
            // ImageMagick-style hex-encoded EXIF/XMP in text chunks: cannot inspect → fail closed
            b"tEXt" | b"zTXt" | b"iTXt" if data.starts_with(b"Raw profile type") => {
                return Err(ImageError::Exif);
            }
            b"eXIf" => check_exif(data)?,
            b"iTXt" if data.starts_with(XMP_PNG) => {
                // keyword\0 compression_flag compression_method lang\0 key\0 text
                if data.get(XMP_PNG.len()) != Some(&0) {
                    return Err(ImageError::Exif); // compressed XMP: cannot inspect → fail closed
                }
                check_xmp(data)?;
            }
            b"IDAT" => idat = true,
            b"IEND" => {
                if !idat {
                    return Err(m("png without IDAT"));
                }
                if next != b.len() {
                    return Err(m("png trailing bytes"));
                }
                return Ok(ImageInfo {
                    kind: ImageKind::Png,
                    width,
                    height,
                });
            }
            _ => {}
        }
        off = next;
    }
}

/// JPEG: walk marker segments. Exactly one SOFn (excl. DHT C4 / JPG C8 / DAC CC) gives the
/// size (length must cover 6 + 3·components). APP1 Exif/XMP are checked. After each SOS the
/// entropy-coded data is skipped to the next real marker (progressive files have several
/// scans). The walk must end exactly at EOI = end of file — decoders stop at the first EOI,
/// so anything after it would be smuggled content.
fn jpeg(b: &[u8]) -> Result<ImageInfo, ImageError> {
    let m = ImageError::Malformed;
    let mut off = 2usize; // after SOI
    let mut size: Option<(u32, u32)> = None;
    let mut scans = 0usize;
    loop {
        while slice(b, off, 2) == Some(&[0xFF, 0xFF]) {
            off += 1; // fill bytes
        }
        if b.get(off) != Some(&0xFF) {
            return Err(m("jpeg marker"));
        }
        let marker = *b.get(off + 1).ok_or(m("jpeg marker"))?;
        off += 2;
        match marker {
            0x01 | 0xD0..=0xD7 => continue,
            0xD8 => return Err(m("jpeg unexpected SOI")),
            0xD9 => {
                let (width, height) = size.ok_or(m("jpeg without SOF"))?;
                if scans == 0 {
                    return Err(m("jpeg without scan"));
                }
                if off != b.len() {
                    return Err(m("jpeg trailing bytes"));
                }
                return Ok(ImageInfo {
                    kind: ImageKind::Jpeg,
                    width,
                    height,
                });
            }
            _ => {}
        }
        let len = be16(b, off).ok_or(m("jpeg segment"))? as usize;
        if len < 2 {
            return Err(m("jpeg segment length"));
        }
        let seg = slice(b, off + 2, len - 2).ok_or(m("jpeg segment length"))?;
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            check_exif(&seg[6..])?;
        }
        if marker == 0xE1 && (seg.starts_with(XMP_JPEG) || seg.starts_with(XMP_EXT_JPEG)) {
            check_xmp(seg)?;
        }
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            if size.is_some() {
                return Err(m("jpeg second SOF"));
            }
            // precision(1) height(2) width(2) ncomp(1) + 3 per component
            let ncomp = *seg.get(5).ok_or(m("jpeg SOF"))? as usize;
            if ncomp == 0 || seg.len() < 6 + 3 * ncomp {
                return Err(m("jpeg SOF"));
            }
            let height = u16::from_be_bytes([seg[1], seg[2]]) as u32;
            let width = u16::from_be_bytes([seg[3], seg[4]]) as u32;
            size = Some((width, height));
        }
        off += len;
        if marker == 0xDA {
            if size.is_none() {
                return Err(m("jpeg SOS before SOF"));
            }
            scans += 1;
            // skip entropy data: FF 00 = stuffed byte, FF D0..D7 = restart, FF FF = fill
            loop {
                if *b.get(off).ok_or(m("jpeg scan"))? == 0xFF {
                    let y = *b.get(off + 1).ok_or(m("jpeg scan"))?;
                    if y != 0x00 && y != 0xFF && !(0xD0..=0xD7).contains(&y) {
                        break;
                    }
                }
                off += 1;
            }
        }
    }
}

/// WebP: RIFF size must match the file; walk chunks. Exactly one VP8/VP8L image; a VP8X
/// canvas must equal it. ANIM/ANMF → Animated · EXIF → `check_exif` · XMP → `check_xmp`.
fn webp(b: &[u8]) -> Result<ImageInfo, ImageError> {
    let m = ImageError::Malformed;
    let riff = le32(b, 4).ok_or(m("webp RIFF"))? as usize;
    if riff.checked_add(8) != Some(b.len()) {
        return Err(m("webp RIFF size"));
    }
    let mut off = 12usize;
    let mut canvas: Option<(u32, u32)> = None;
    let mut frame: Option<(u32, u32)> = None;
    while off < b.len() {
        let kind = slice(b, off, 4).ok_or(m("webp chunk"))?;
        let len = le32(b, off + 4).ok_or(m("webp chunk"))? as usize;
        let data = off.checked_add(8).ok_or(m("webp chunk"))?;
        // checked: on wasm32 `len + 1` wraps for len = u32::MAX
        let padded = len.checked_add(len & 1).ok_or(m("webp chunk length"))?;
        let next = data
            .checked_add(padded)
            .filter(|n| *n <= b.len())
            .ok_or(m("webp chunk length"))?;
        let d = slice(b, data, len).ok_or(m("webp chunk length"))?;
        match kind {
            b"ANIM" | b"ANMF" => return Err(ImageError::Animated),
            b"EXIF" => check_exif(d)?,
            b"XMP " => check_xmp(d)?,
            b"VP8X" => {
                let flags = *d.first().ok_or(m("webp VP8X"))?;
                if flags & 0x02 != 0 {
                    return Err(ImageError::Animated);
                }
                let w = le24(d, 4).ok_or(m("webp VP8X"))? + 1;
                let h = le24(d, 7).ok_or(m("webp VP8X"))? + 1;
                if canvas.replace((w, h)).is_some() {
                    return Err(m("webp second VP8X"));
                }
            }
            b"VP8 " => {
                if slice(d, 3, 3) != Some(&[0x9d, 0x01, 0x2a]) {
                    return Err(m("webp VP8"));
                }
                let w = (le16(d, 6).ok_or(m("webp VP8"))? & 0x3FFF) as u32;
                let h = (le16(d, 8).ok_or(m("webp VP8"))? & 0x3FFF) as u32;
                if frame.replace((w, h)).is_some() {
                    return Err(m("webp second image"));
                }
            }
            b"VP8L" => {
                if d.first() != Some(&0x2F) {
                    return Err(m("webp VP8L"));
                }
                let bits = le32(d, 1).ok_or(m("webp VP8L"))?;
                let wh = ((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1);
                if frame.replace(wh).is_some() {
                    return Err(m("webp second image"));
                }
            }
            _ => {}
        }
        off = next;
    }
    let (width, height) = frame.ok_or(m("webp without image data"))?;
    // decoders reject a still image whose frame differs from the VP8X canvas
    if canvas.is_some_and(|c| c != (width, height)) {
        return Err(m("webp canvas != frame"));
    }
    Ok(ImageInfo {
        kind: ImageKind::Webp,
        width,
        height,
    })
}

/// Type + dimensions + structure. Never trusts anything but the bytes.
pub fn sniff(b: &[u8]) -> Result<ImageInfo, ImageError> {
    if b.is_empty() {
        return Err(ImageError::Empty);
    }
    let info = if b.starts_with(&PNG_SIG) {
        png(b)?
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        jpeg(b)?
    } else if slice(b, 0, 4) == Some(b"RIFF") && slice(b, 8, 4) == Some(b"WEBP") {
        webp(b)?
    } else {
        return Err(ImageError::UnknownType);
    };
    if info.width == 0 || info.height == 0 {
        return Err(ImageError::ZeroSize);
    }
    Ok(info)
}

/// `sniff` + byte cap + exact slot size.
pub fn check(b: &[u8], width: u16, height: u16, max_bytes: usize) -> Result<ImageInfo, ImageError> {
    if b.len() > max_bytes {
        return Err(ImageError::TooLarge {
            max: max_bytes,
            got: b.len(),
        });
    }
    let info = sniff(b)?;
    let expected = (width as u32, height as u32);
    if (info.width, info.height) != expected {
        return Err(ImageError::WrongSize {
            expected,
            got: (info.width, info.height),
        });
    }
    Ok(info)
}
