//! Bounded clipboard raster conversion. Only decoded pixels cross into native
//! bitmap formats; peer metadata, profiles and file paths are never published.
use anyhow::{bail, ensure, Context, Result};
use emc_protocol::{MAX_CLIPBOARD_IMAGE_BYTES, MAX_CLIPBOARD_IMAGE_RGBA_BYTES};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
pub const MAX_DIB_BYTES: usize = MAX_CLIPBOARD_IMAGE_RGBA_BYTES + 124;
pub const MAX_CACHED_IMAGES: usize = 4;
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    digest: [u8; 32],
}

fn rgba_size(width: u32, height: u32) -> Result<usize> {
    ensure!(
        width > 0 && height > 0 && width <= MAX_IMAGE_DIMENSION && height <= MAX_IMAGE_DIMENSION,
        "Clipboard image dimensions are unsupported"
    );
    let size = u64::from(width) * u64::from(height) * 4;
    ensure!(
        size <= MAX_CLIPBOARD_IMAGE_RGBA_BYTES as u64,
        "Clipboard image exceeds its decoded limit"
    );
    Ok(size as usize)
}

impl ClipboardImage {
    /// Import pixels from one copied local image file. Its original path/name
    /// and metadata are not part of the image transfer.
    pub fn from_file_bytes(bytes: &[u8]) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES,
            "Copied image file exceeds its encoded limit"
        );
        match image::guess_format(bytes)? {
            image::ImageFormat::Png => Self::from_png(bytes),
            image::ImageFormat::Jpeg => {
                decode_file_image(image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes))?)
            }
            image::ImageFormat::WebP => {
                validate_webp_file(bytes)?;
                let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?;
                ensure!(
                    !decoder.has_animation(),
                    "Animated image files are unsupported"
                );
                decode_file_image(decoder)
            }
            _ => bail!("Unsupported copied image file format"),
        }
    }
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self> {
        ensure!(
            rgba.len() == rgba_size(width, height)?,
            "Invalid clipboard image pixel length"
        );
        let mut hash = Sha256::new();
        hash.update(b"EMC.clipboard.rgba8.v1");
        hash.update(width.to_le_bytes());
        hash.update(height.to_le_bytes());
        hash.update(&rgba);
        Ok(Self {
            width,
            height,
            rgba,
            digest: hash.finalize().into(),
        })
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn from_png(bytes: &[u8]) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES,
            "Clipboard PNG exceeds its encoded limit"
        );
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_limits(png::Limits {
            bytes: MAX_CLIPBOARD_IMAGE_RGBA_BYTES,
        });
        decoder.set_ignore_text_chunk(true);
        decoder.set_ignore_iccp_chunk(true);
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info()?;
        let (width, height) = (reader.info().width, reader.info().height);
        let size = rgba_size(width, height)?;
        ensure!(
            reader.info().animation_control.is_none(),
            "Animated clipboard PNG is unsupported"
        );
        let output_size = reader
            .output_buffer_size()
            .context("Clipboard PNG output size is invalid")?;
        ensure!(
            output_size <= size,
            "Clipboard PNG output exceeds its decoded limit"
        );
        let mut pixels = vec![0; output_size];
        let frame = reader.next_frame(&mut pixels)?;
        ensure!(
            frame.width == width
                && frame.height == height
                && frame.bit_depth == png::BitDepth::Eight,
            "Unsupported clipboard PNG frame"
        );
        pixels.truncate(frame.buffer_size());
        reader.finish()?;
        let mut rgba = Vec::with_capacity(size);
        match frame.color_type {
            png::ColorType::Rgba => rgba = pixels,
            png::ColorType::Rgb => {
                for pixel in pixels.as_chunks::<3>().0 {
                    rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
            }
            png::ColorType::Grayscale => {
                for value in pixels {
                    rgba.extend_from_slice(&[value, value, value, 255]);
                }
            }
            png::ColorType::GrayscaleAlpha => {
                for pixel in pixels.as_chunks::<2>().0 {
                    rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
                }
            }
            _ => bail!("Unsupported clipboard PNG color type"),
        }
        Self::new(width, height, rgba)
    }
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let mut output = BoundedPng(Vec::new());
        {
            let mut encoder = png::Encoder::new(&mut output, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&self.rgba)?;
            writer.finish()?;
        }
        Ok(output.0)
    }
    /// CF_DIB and CF_DIBV5 are packed DIBs, without a BITMAPFILEHEADER. Support
    /// ordinary uncompressed 24/32-bit pixels and the standard BGRA bitfields.
    pub fn from_dib(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_DIB_BYTES,
            "Clipboard DIB exceeds its decoded limit"
        );
        let header = read_u32(bytes, 0)? as usize;
        ensure!(
            matches!(header, 40 | 108 | 124) && bytes.len() >= header,
            "Unsupported clipboard DIB header"
        );
        let width = read_u32(bytes, 4)? as i32;
        let signed_height = read_u32(bytes, 8)? as i32;
        ensure!(width > 0, "Invalid clipboard DIB width");
        let height = signed_height
            .checked_abs()
            .context("Invalid clipboard DIB height")? as u32;
        let size = rgba_size(width as u32, height)?;
        ensure!(
            read_u16(bytes, 12)? == 1 && read_u32(bytes, 32)? == 0,
            "Unsupported clipboard DIB planes or palette"
        );
        let depth = read_u16(bytes, 14)?;
        let compression = read_u32(bytes, 16)?;
        ensure!(
            matches!(depth, 24 | 32) && (compression == 0 || (compression == 3 && depth == 32)),
            "Compressed or indexed clipboard DIB is unsupported"
        );
        if header == 124 {
            ensure!(
                !matches!(read_u32(bytes, 56)?, 0x4c49_4e4b | 0x4d42_4544)
                    && read_u32(bytes, 112)? == 0
                    && read_u32(bytes, 116)? == 0,
                "Clipboard DIB color profiles are unsupported"
            );
        }
        let mut offset = header;
        let alpha = if compression == 3 {
            let masks = if header == 40 {
                offset += 12;
                40
            } else {
                40
            };
            ensure!(
                read_u32(bytes, masks)? == 0x00ff_0000
                    && read_u32(bytes, masks + 4)? == 0x0000_ff00
                    && read_u32(bytes, masks + 8)? == 0x0000_00ff,
                "Unsupported clipboard DIB masks"
            );
            if header >= 108 {
                let mask = read_u32(bytes, 52)?;
                ensure!(
                    mask == 0 || mask == 0xff00_0000,
                    "Unsupported clipboard DIB alpha mask"
                );
                mask != 0
            } else {
                false
            }
        } else {
            false
        };
        let row = (width as usize * depth as usize).div_ceil(32) * 4;
        let end = offset
            .checked_add(
                row.checked_mul(height as usize)
                    .context("Invalid clipboard DIB length")?,
            )
            .context("Invalid clipboard DIB length")?;
        ensure!(end <= bytes.len(), "Truncated clipboard DIB pixels");
        let mut rgba = Vec::with_capacity(size);
        for y in 0..height as usize {
            let source_y = if signed_height < 0 {
                y
            } else {
                height as usize - 1 - y
            };
            let source = &bytes[offset + source_y * row..offset + (source_y + 1) * row];
            for pixel in
                source[..width as usize * (depth as usize / 8)].chunks_exact(depth as usize / 8)
            {
                rgba.extend_from_slice(&[
                    pixel[2],
                    pixel[1],
                    pixel[0],
                    if alpha { pixel[3] } else { 255 },
                ]);
            }
        }
        Self::new(width as u32, height, rgba)
    }
    pub fn to_dib_v5(&self) -> Vec<u8> {
        let mut bytes = vec![0; 124];
        put_u32(&mut bytes, 0, 124);
        put_u32(&mut bytes, 4, self.width);
        put_u32(&mut bytes, 8, (-(self.height as i32)) as u32);
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
        put_u32(&mut bytes, 16, 3);
        put_u32(&mut bytes, 20, self.rgba.len() as u32);
        for (offset, mask) in [
            (40, 0x00ff_0000),
            (44, 0x0000_ff00),
            (48, 0x0000_00ff),
            (52, 0xff00_0000),
            (56, 0x7352_4742),
            (108, 4),
        ] {
            put_u32(&mut bytes, offset, mask);
        }
        for pixel in self.rgba.as_chunks::<4>().0 {
            bytes.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        bytes
    }
    fn to_dib(&self) -> Vec<u8> {
        let row = (self.width as usize * 3).div_ceil(4) * 4;
        let mut bytes = vec![0; 40 + row * self.height as usize];
        put_u32(&mut bytes, 0, 40);
        put_u32(&mut bytes, 4, self.width);
        put_u32(&mut bytes, 8, self.height);
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&24u16.to_le_bytes());
        for y in 0..self.height as usize {
            for x in 0..self.width as usize {
                let pixel = &self.rgba[(y * self.width as usize + x) * 4..][..4];
                let offset = 40 + (self.height as usize - 1 - y) * row + x * 3;
                // Legacy DIB has no alpha channel: flatten onto white while the
                // PNG/DIBV5 representations retain the original alpha.
                for (i, component) in [pixel[2], pixel[1], pixel[0]].into_iter().enumerate() {
                    bytes[offset + i] = ((u16::from(component) * u16::from(pixel[3])
                        + 255 * (255 - u16::from(pixel[3]))
                        + 127)
                        / 255) as u8;
                }
            }
        }
        bytes
    }
}

/// Check RIFF ranges before a decoder can allocate a declared metadata chunk.
/// Extended WebP also carries a canvas size separate from its pixel bitstream;
/// both must meet the decoded limit before the codec reads pixel data.
fn validate_webp_file(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "Invalid WebP container"
    );
    let end = (read_u32(bytes, 4)? as usize)
        .checked_add(8)
        .context("Invalid WebP size")?;
    ensure!(
        end == bytes.len(),
        "Truncated or inconsistent WebP container"
    );
    let mut at = 12usize;
    let mut dimensions = None;
    while at < end {
        ensure!(end - at >= 8, "Truncated WebP chunk");
        let kind = &bytes[at..at + 4];
        let size = read_u32(bytes, at + 4)? as usize;
        let start = at + 8;
        let finish = start.checked_add(size).context("Invalid WebP chunk size")?;
        let next = finish
            .checked_add(size & 1)
            .context("Invalid WebP chunk padding")?;
        ensure!(next <= end, "WebP chunk exceeds actual input");
        let chunk = &bytes[start..finish];
        let parsed = match kind {
            b"VP8X" => {
                ensure!(
                    size == 10 && chunk[0] & 2 == 0,
                    "Animated or invalid WebP canvas"
                );
                Some((
                    u32::from_le_bytes([chunk[4], chunk[5], chunk[6], 0]) + 1,
                    u32::from_le_bytes([chunk[7], chunk[8], chunk[9], 0]) + 1,
                ))
            }
            b"VP8L" => {
                ensure!(
                    size >= 5 && chunk[0] == 0x2f,
                    "Invalid lossless WebP header"
                );
                let bits = read_u32(chunk, 1)?;
                ensure!(bits >> 29 == 0, "Unsupported lossless WebP version");
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8 " => {
                ensure!(
                    size >= 10 && chunk[0] & 1 == 0 && chunk[3..6] == [0x9d, 1, 0x2a],
                    "Invalid lossy WebP header"
                );
                Some((
                    u32::from(u16::from_le_bytes([chunk[6], chunk[7]]) & 0x3fff),
                    u32::from(u16::from_le_bytes([chunk[8], chunk[9]]) & 0x3fff),
                ))
            }
            b"ANIM" | b"ANMF" => bail!("Animated WebP is unsupported"),
            _ => None,
        };
        if let Some(size) = parsed {
            rgba_size(size.0, size.1)?;
            ensure!(
                dimensions.is_none_or(|previous| previous == size),
                "WebP canvas and pixel sizes differ"
            );
            dimensions = Some(size);
        }
        at = next;
    }
    ensure!(dimensions.is_some(), "Missing WebP pixels");
    Ok(())
}

fn decode_file_image(mut decoder: impl image::ImageDecoder) -> Result<ClipboardImage> {
    let (width, height) = decoder.dimensions();
    rgba_size(width, height)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_CLIPBOARD_IMAGE_RGBA_BYTES as u64);
    decoder.set_limits(limits)?;
    let orientation = decoder.orientation()?;
    let mut decoded = image::DynamicImage::from_decoder(decoder)?;
    decoded.apply_orientation(orientation);
    let pixels = decoded.into_rgba8();
    ClipboardImage::new(pixels.width(), pixels.height(), pixels.into_raw())
}

#[cfg(any(windows, test))]
fn external_image(
    image: ClipboardImage,
    received_origin: Option<[u8; 32]>,
) -> Option<ClipboardImage> {
    (received_origin != Some(image.digest())).then_some(image)
}

/// DROPFILES data is copied while the native clipboard is locked. Parse only
/// bounded UTF-16 paths; never interpret them as peer paths or commands.
#[cfg(any(windows, test))]
fn drop_paths(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    ensure!(
        bytes.len() >= 24 && bytes.len() <= 64 * 1024,
        "Invalid file clipboard size"
    );
    ensure!(
        read_u32(bytes, 16)? == 1,
        "Unsupported file clipboard format"
    );
    let offset = read_u32(bytes, 0)? as usize;
    ensure!(
        offset >= 20 && offset <= bytes.len() && (bytes.len() - offset).is_multiple_of(2),
        "Invalid file clipboard format"
    );
    let units: Vec<_> = bytes[offset..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    ensure!(
        units.ends_with(&[0, 0]),
        "Unterminated file clipboard format"
    );
    units
        .split(|unit| *unit == 0)
        .filter(|part| !part.is_empty())
        .map(|part| Ok(PathBuf::from(String::from_utf16(part)?)))
        .collect()
}

#[cfg(any(windows, test))]
fn is_local_image_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    let bytes = text.as_bytes();
    // Drive-qualified local paths only. No UNC, device namespace, relative
    // path, alternate data stream or a list of arbitrary files.
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'\\'
        && !text[2..].contains(':')
        && text
            .rsplit(['\\', '/'])
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .is_some_and(|(_, extension)| {
                ["png", "jpg", "jpeg", "webp"]
                    .iter()
                    .any(|supported| extension.eq_ignore_ascii_case(supported))
            })
}

#[cfg(any(windows, test))]
fn image_drop_path(bytes: &[u8]) -> Result<Option<PathBuf>> {
    let mut paths = drop_paths(bytes)?;
    if paths.len() == 1 && is_local_image_path(&paths[0]) {
        Ok(paths.pop())
    } else {
        Ok(None)
    }
}

struct BoundedPng(Vec<u8>);
impl Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_CLIPBOARD_IMAGE_BYTES {
            return Err(std::io::Error::other(
                "Clipboard PNG exceeds its encoded limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .context("Truncated clipboard DIB header")?
            .try_into()?,
    ))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("Truncated clipboard DIB header")?
            .try_into()?,
    ))
}
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Conversion and temporary-file I/O run before the engine takes its commit
/// lock. Drop removes an uncommitted image after a policy/session change.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct PreparedImage {
    png: Vec<u8>,
    dib_v5: Vec<u8>,
    dib: Vec<u8>,
    digest: [u8; 32],
    cache: PathBuf,
    temporary: Option<PathBuf>,
}
impl PreparedImage {
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
impl Drop for PreparedImage {
    fn drop(&mut self) {
        if let Some(path) = self.temporary.take() {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn prepare_image(image: ClipboardImage, cache: &Path) -> Result<PreparedImage> {
    ensure!(
        cache.is_absolute(),
        "Clipboard image cache must be absolute"
    );
    fs::create_dir_all(cache)?;
    ensure!(
        !fs::symlink_metadata(cache)?.file_type().is_symlink(),
        "Invalid clipboard image cache"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(cache, fs::Permissions::from_mode(0o700))?;
    }
    for (index, entry) in fs::read_dir(cache)?.enumerate() {
        ensure!(
            index < 64,
            "Clipboard image cache contains too many entries"
        );
        let entry = entry?;
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(".image-") && name.ends_with(".part"))
        {
            let metadata = fs::symlink_metadata(entry.path())?;
            // A crashed preparation cannot grow a second unbounded set of
            // images. Never delete a live/unknown temporary file; refuse until
            // the previous generated temporary file is old enough to retire.
            let generated = name
                .to_str()
                .and_then(|name| name.strip_prefix(".image-")?.strip_suffix(".part"))
                .is_some_and(|ids| {
                    ids.split_once('-').is_some_and(|(process, serial)| {
                        !process.is_empty()
                            && !serial.is_empty()
                            && process.bytes().all(|b| b.is_ascii_digit())
                            && serial.bytes().all(|b| b.is_ascii_digit())
                    })
                });
            if generated && metadata.is_file() && !metadata.file_type().is_symlink() {
                ensure!(
                    metadata.modified()?.elapsed().unwrap_or_default()
                        > std::time::Duration::from_secs(300),
                    "A clipboard image preparation is already pending"
                );
                fs::remove_file(entry.path())?;
            }
        }
    }
    let png = image.to_png()?;
    let digest = image.digest();
    let temporary = cache.join(format!(
        ".image-{}-{}.part",
        std::process::id(),
        TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut prepared = PreparedImage {
        png,
        dib_v5: image.to_dib_v5(),
        dib: image.to_dib(),
        digest,
        cache: cache.to_path_buf(),
        temporary: Some(temporary.clone()),
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Set the cleanup path only after create_new succeeds: an existing temporary
    // file belongs to an earlier process and must never be deleted here.
    prepared.temporary = None;
    let mut file = options.open(&temporary)?;
    prepared.temporary = Some(temporary);
    file.write_all(&prepared.png)?;
    file.sync_all()?;
    Ok(prepared)
}

#[cfg(any(windows, test))]
fn managed_name(name: &str) -> bool {
    name.len() == 68
        && name.ends_with(".png")
        && name.as_bytes()[..64]
            .iter()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
#[cfg(any(windows, test))]
struct CachePlan {
    destination: PathBuf,
    needs_rename: bool,
}
#[cfg(any(windows, test))]
fn prepare_cache(
    prepared: &PreparedImage,
    referenced: &[PathBuf],
    mark: impl FnOnce(&Path) -> Result<()>,
) -> Result<CachePlan> {
    let encoded_hash: [u8; 32] = Sha256::digest(&prepared.png).into();
    let name: String = encoded_hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let destination = prepared.cache.join(format!("{name}.png"));
    let mut entries = vec![];
    for (index, entry) in fs::read_dir(&prepared.cache)?.enumerate() {
        ensure!(
            index < 64,
            "Clipboard image cache contains too many entries"
        );
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if entry.file_name().to_str().is_some_and(managed_name)
            && metadata.is_file()
            && !metadata.file_type().is_symlink()
        {
            ensure!(
                metadata.len() <= MAX_CLIPBOARD_IMAGE_BYTES as u64,
                "Invalid managed clipboard image size"
            );
            entries.push((
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                entry.path(),
            ));
        }
    }
    entries.sort();
    let exists = entries.iter().any(|(_, path)| path == &destination);
    let target_count = if exists {
        MAX_CACHED_IMAGES
    } else {
        MAX_CACHED_IMAGES - 1
    };
    while entries.len() > target_count {
        let position = entries
            .iter()
            .position(|(_, path)| {
                path != &destination
                    && !referenced.iter().any(|held| {
                        path.to_string_lossy()
                            .eq_ignore_ascii_case(&held.to_string_lossy())
                    })
            })
            .context("Clipboard image cache is full of referenced images")?;
        fs::remove_file(entries.remove(position).1)?;
    }
    if exists {
        let actual: [u8; 32] = Sha256::digest(fs::read(&destination)?).into();
        ensure!(
            actual == encoded_hash,
            "Cached clipboard image integrity mismatch"
        );
        mark(&destination)?;
    } else {
        let temporary = prepared
            .temporary
            .as_ref()
            .context("Missing clipboard image temporary file")?;
        mark(temporary)?;
    }
    Ok(CachePlan {
        destination,
        needs_rename: !exists,
    })
}

#[cfg(any(windows, test))]
fn finish_cache(
    prepared: &mut PreparedImage,
    plan: &CachePlan,
    rename: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<()> {
    if plan.needs_rename {
        let temporary = prepared
            .temporary
            .as_ref()
            .context("Missing clipboard image temporary file")?;
        rename(temporary, &plan.destination)?;
        prepared.temporary = None;
    }
    Ok(())
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{prepare_publication, read_image, read_image_sample};
#[cfg(not(windows))]
pub fn read_image() -> Result<Option<ClipboardImage>> {
    read_image_sample().map(|(_, image)| image)
}
#[cfg(not(windows))]
pub fn read_image_sample() -> Result<(u32, Option<ClipboardImage>)> {
    Ok((0, None))
}
#[cfg(not(windows))]
pub struct PreparedPublication;
#[cfg(not(windows))]
impl PreparedPublication {
    pub fn publish(&mut self) -> Result<()> {
        bail!("Clipboard images require the Windows desktop backend")
    }
}
#[cfg(not(windows))]
pub fn prepare_publication(_: PreparedImage) -> Result<PreparedPublication> {
    bail!("Clipboard images require the Windows desktop backend")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image() -> ClipboardImage {
        ClipboardImage::new(2, 1, vec![255, 0, 0, 255, 0, 100, 200, 127]).unwrap()
    }

    #[test]
    fn copied_png_jpeg_and_webp_files_decode_to_bounded_pixels() {
        let source = image();
        assert_eq!(
            ClipboardImage::from_file_bytes(&source.to_png().unwrap()).unwrap(),
            source
        );
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode(
                &[255, 0, 0, 0, 100, 200],
                2,
                1,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        let decoded = ClipboardImage::from_file_bytes(&jpeg).unwrap();
        assert_eq!(
            (decoded.width, decoded.height, decoded.rgba.len()),
            (2, 1, 8)
        );
        assert!(decoded
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255));
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .encode(&source.rgba, 2, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert_eq!(ClipboardImage::from_file_bytes(&webp).unwrap(), source);

        // Reject large declared dimensions from headers before allocating the
        // decoded pixel buffer, independently of the small compressed input.
        let sof = jpeg
            .windows(2)
            .position(|pair| pair == [0xff, 0xc0])
            .unwrap();
        jpeg[sof + 5..sof + 9].fill(0xff);
        assert!(ClipboardImage::from_file_bytes(&jpeg).is_err());
        for invalid in [
            b"".as_slice(),
            b"GIF89a",
            b"not an image",
            b"RIFF\x00\x00\x00\x00WEBP",
        ] {
            assert!(ClipboardImage::from_file_bytes(invalid).is_err());
        }
        assert!(ClipboardImage::from_file_bytes(&vec![0; MAX_CLIPBOARD_IMAGE_BYTES + 1]).is_err());
    }

    #[test]
    fn received_marker_ignores_retained_pixels_but_allows_a_new_copy_or_changed_image() {
        let received = image();
        let origin = received.digest();
        // Shell-only metadata changes retain our marker. EmptyClipboard on a
        // new Copy removes it; changing the pixels also defeats an old marker.
        assert!(external_image(received.clone(), Some(origin)).is_none());
        assert_eq!(external_image(received.clone(), None), Some(received));
        let changed = ClipboardImage::new(1, 1, vec![0, 0, 0, 255]).unwrap();
        assert_eq!(external_image(changed.clone(), Some(origin)), Some(changed));
    }

    #[test]
    fn webp_container_rejects_overdeclared_metadata_and_mismatched_pixel_sizes_before_decode() {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(kind);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            if !data.len().is_multiple_of(2) {
                out.push(0);
            }
        }
        let source = image();
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .encode(&source.rgba, 2, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let mut extended = b"RIFF\0\0\0\0WEBP".to_vec();
        chunk(&mut extended, b"VP8X", &[8, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
        extended.extend_from_slice(&webp[12..]);
        extended.extend_from_slice(b"EXIF");
        extended.extend_from_slice(&0x7fff_fffeu32.to_le_bytes());
        let length = extended.len() as u32 - 8;
        put_u32(&mut extended, 4, length);
        assert_eq!(
            validate_webp_file(&extended).unwrap_err().to_string(),
            "WebP chunk exceeds actual input"
        );
        // Exercise production dispatch with a small allocation if the guard is
        // ever removed; the multi-gigabyte declaration above stays validator-only.
        let metadata_length = extended.len() - 4;
        put_u32(&mut extended, metadata_length, 4096);
        assert_eq!(
            ClipboardImage::from_file_bytes(&extended)
                .unwrap_err()
                .to_string(),
            "WebP chunk exceeds actual input"
        );

        let mut mismatched = b"RIFF\0\0\0\0WEBP".to_vec();
        chunk(&mut mismatched, b"VP8X", &[0; 10]);
        let mut pixels = [0, 0, 0, 0x9d, 1, 0x2a, 0, 0, 0, 0];
        pixels[6..8].copy_from_slice(&16_383u16.to_le_bytes());
        pixels[8..10].copy_from_slice(&16_383u16.to_le_bytes());
        chunk(&mut mismatched, b"VP8 ", &pixels);
        let length = mismatched.len() as u32 - 8;
        put_u32(&mut mismatched, 4, length);
        assert!(validate_webp_file(&mismatched).is_err());
        let pixel_header = mismatched.len() - 4;
        mismatched[pixel_header..pixel_header + 2].copy_from_slice(&32u16.to_le_bytes());
        mismatched[pixel_header + 2..pixel_header + 4].copy_from_slice(&32u16.to_le_bytes());
        assert_eq!(
            ClipboardImage::from_file_bytes(&mismatched)
                .unwrap_err()
                .to_string(),
            "WebP canvas and pixel sizes differ"
        );
        let mut truncated = webp;
        truncated.pop();
        assert!(ClipboardImage::from_file_bytes(&truncated).is_err());
    }

    fn drop_fixture(paths: &[&str]) -> Vec<u8> {
        let mut bytes = vec![0; 20];
        put_u32(&mut bytes, 0, 20);
        put_u32(&mut bytes, 16, 1);
        for path in paths {
            for unit in path.encode_utf16().chain(Some(0)) {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn explorer_import_selects_one_local_supported_image_and_rejects_other_lists() {
        for path in [
            r"C:\Images\photo.png",
            r"c:\Images\my photo.JPEG",
            r"D:\Images\图片.webp",
        ] {
            assert_eq!(
                image_drop_path(&drop_fixture(&[path])).unwrap(),
                Some(PathBuf::from(path))
            );
        }
        for path in [
            r"\\server\share\photo.png",
            r"\\?\C:\photo.png",
            r"C:photo.png",
            r"C:\photo.png:stream",
            r"C:\document.txt",
            r"C:\folder",
            "/tmp/photo.jpg",
        ] {
            assert_eq!(image_drop_path(&drop_fixture(&[path])).unwrap(), None);
        }
        assert!(
            image_drop_path(&drop_fixture(&[r"C:\one.png", r"C:\two.jpg"]))
                .unwrap()
                .is_none()
        );
        let mut invalid = drop_fixture(&[r"C:\one.png"]);
        invalid.truncate(invalid.len() - 3);
        assert!(image_drop_path(&invalid).is_err());
        let mut invalid = drop_fixture(&[r"C:\one.png"]);
        put_u32(&mut invalid, 0, u32::MAX);
        assert!(image_drop_path(&invalid).is_err());
    }
    #[test]
    fn png_and_dib_v5_roundtrip_pixels_and_echo_digest() {
        let original = image();
        assert_eq!(
            ClipboardImage::from_png(&original.to_png().unwrap()).unwrap(),
            original
        );
        assert_eq!(
            ClipboardImage::from_dib(&original.to_dib_v5()).unwrap(),
            original
        );
        assert_eq!(
            ClipboardImage::from_png(&original.to_png().unwrap())
                .unwrap()
                .digest(),
            original.digest()
        );
    }
    #[test]
    fn image_dimensions_and_truncated_compressed_profile_dibs_fail_closed() {
        for (width, height) in [(0, 1), (1, 0), (16_385, 1), (8192, 8192)] {
            assert!(ClipboardImage::new(width, height, vec![]).is_err());
        }
        let dib = image().to_dib_v5();
        for length in [0, 4, 40, 123, dib.len() - 1] {
            assert!(ClipboardImage::from_dib(&dib[..length]).is_err());
        }
        for (offset, value) in [
            (8, i32::MIN as u32),
            (16, 1),
            (40, 0x1234),
            (56, 0x4c49_4e4b),
            (112, 124),
        ] {
            let mut invalid = dib.clone();
            put_u32(&mut invalid, offset, value);
            assert!(ClipboardImage::from_dib(&invalid).is_err());
        }
        let mut png = image().to_png().unwrap();
        png.truncate(png.len() - 8);
        assert!(ClipboardImage::from_png(&png).is_err());
        assert!(ClipboardImage::from_png(&vec![0; MAX_CLIPBOARD_IMAGE_BYTES + 1]).is_err());
    }
    #[test]
    fn png_header_dimensions_and_animation_are_checked_before_pixel_allocation() {
        let mut oversized = vec![];
        {
            let mut encoder = png::Encoder::new(&mut oversized, MAX_IMAGE_DIMENSION + 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![0; (MAX_IMAGE_DIMENSION as usize + 1) * 4])
                .unwrap();
            writer.finish().unwrap();
        }
        assert!(ClipboardImage::from_png(&oversized).is_err());
        let mut animated = vec![];
        {
            let mut encoder = png::Encoder::new(&mut animated, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_animated(1, 0).unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 0, 0, 255]).unwrap();
            writer.finish().unwrap();
        }
        assert!(ClipboardImage::from_png(&animated).is_err());
    }
    #[test]
    fn legacy_dib_padding_bottom_up_rows_and_opaque_alpha_are_supported() {
        let image = ClipboardImage::new(1, 2, vec![10, 20, 30, 255, 40, 50, 60, 255]).unwrap();
        assert_eq!(ClipboardImage::from_dib(&image.to_dib()).unwrap(), image);
    }
    struct Cache(PathBuf);
    impl Cache {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "emc-image-test-{}-{}",
                std::process::id(),
                TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Cache {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn cache_is_bounded_excludes_current_clipboard_and_never_removes_unmanaged_files() {
        let cache = Cache::new();
        fs::write(
            cache.0.join("unrelated.txt"),
            b"synthetic unmanaged fixture",
        )
        .unwrap();
        let mut held = None;
        for n in 0..8 {
            let image = ClipboardImage::new(1, 1, vec![n, 0, 0, 255]).unwrap();
            let mut prepared = prepare_image(image, &cache.0).unwrap();
            let pinned: Vec<_> = held.iter().cloned().collect();
            let plan = prepare_cache(&prepared, &pinned, |_| Ok(())).unwrap();
            finish_cache(&mut prepared, &plan, |from, to| Ok(fs::rename(from, to)?)).unwrap();
            if held.is_none() {
                held = Some(plan.destination);
            }
            assert!(held.as_ref().unwrap().exists());
            assert!(
                fs::read_dir(&cache.0)
                    .unwrap()
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.file_name().to_str().is_some_and(managed_name))
                    .count()
                    <= MAX_CACHED_IMAGES
            );
        }
        assert_eq!(
            fs::read(cache.0.join("unrelated.txt")).unwrap(),
            b"synthetic unmanaged fixture"
        );
        assert!(!managed_name(&format!("{}.png", "é".repeat(32))));
    }
    #[test]
    fn cancelled_or_unmarked_preparation_never_leaves_a_final_png() {
        let cache = Cache::new();
        let temporary = {
            let prepared = prepare_image(image(), &cache.0).unwrap();
            let temporary = prepared.temporary.clone().unwrap();
            assert!(prepare_cache(&prepared, &[], |_| bail!("synthetic marking refusal")).is_err());
            assert!(temporary.exists());
            temporary
        };
        assert!(!temporary.exists());
        assert_eq!(fs::read_dir(&cache.0).unwrap().count(), 0);
    }
    #[test]
    fn cache_refuses_existing_modified_png_and_preserves_foreign_symlinks() {
        let cache = Cache::new();
        let mut prepared = prepare_image(image(), &cache.0).unwrap();
        let plan = prepare_cache(&prepared, &[], |_| Ok(())).unwrap();
        finish_cache(&mut prepared, &plan, |from, to| Ok(fs::rename(from, to)?)).unwrap();
        drop(prepared);
        fs::write(&plan.destination, b"synthetic cache corruption").unwrap();
        let repeated = prepare_image(image(), &cache.0).unwrap();
        assert!(prepare_cache(&repeated, &[], |_| Ok(())).is_err());
        assert_eq!(
            fs::read(&plan.destination).unwrap(),
            b"synthetic cache corruption"
        );
        #[cfg(unix)]
        {
            let outside = Cache::new();
            let foreign = outside.0.join("foreign.txt");
            fs::write(&foreign, b"synthetic outside content").unwrap();
            let link = cache.0.join(format!("{}.png", "f".repeat(64)));
            std::os::unix::fs::symlink(&foreign, &link).unwrap();
            assert!(prepare_cache(&repeated, &[], |_| Ok(())).is_err());
            assert!(fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read(&foreign).unwrap(), b"synthetic outside content");
        }
    }
}
