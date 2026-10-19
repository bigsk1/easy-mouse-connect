//! Native image clipboard formats and single local image-file import. Paths
//! stay local; only decoded pixels enter the authenticated image transfer.
use super::*;
use std::{
    io::Read,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    ptr::{null, null_mut},
    sync::{Mutex, MutexGuard},
};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    Storage::FileSystem::{
        GetDriveTypeW, GetFinalPathNameByHandleW, MoveFileExW, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_OPEN_REPARSE_POINT,
    },
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
            IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
        },
        LibraryLoader::GetModuleHandleW,
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE},
};

const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;
const CF_HDROP: u32 = 15;
static CACHE_COMMIT: Mutex<()> = Mutex::new(());
const IMAGE_ORIGIN_FORMAT: &str = "EasyMouseConnect.ReceivedImage.v1";

enum Source {
    Pixels(bool, Vec<u8>),
    File(PathBuf),
}

fn read_local_image(path: &Path) -> Result<ClipboardImage> {
    ensure!(
        is_local_image_path(path),
        "Copied image path is unsupported"
    );
    let text = path.to_str().context("Copied image path is unsupported")?;
    let drive: Vec<u16> = text[..3].encode_utf16().chain(Some(0)).collect();
    ensure!(
        matches!(unsafe { GetDriveTypeW(drive.as_ptr()) }, 2 | 3 | 6),
        "Copied image must be on a local drive"
    );
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "Copied image must be a regular local file"
    );
    ensure!(
        metadata.len() > 0 && metadata.len() <= MAX_CLIPBOARD_IMAGE_BYTES as u64,
        "Copied image file exceeds its encoded limit"
    );
    // Check the opened handle's final destination as well as the clipboard path:
    // a local-looking junction must not supply network file contents.
    let mut final_path = vec![0u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            final_path.as_mut_ptr(),
            final_path.len() as u32,
            0,
        )
    };
    ensure!(
        length > 0 && (length as usize) < final_path.len(),
        "Cannot inspect copied image destination"
    );
    let final_path = String::from_utf16(&final_path[..length as usize])?;
    let final_path = final_path.strip_prefix("\\\\?\\").unwrap_or(&final_path);
    ensure!(
        is_local_image_path(Path::new(final_path)),
        "Copied image must be a local file"
    );
    let drive: Vec<u16> = final_path[..3].encode_utf16().chain(Some(0)).collect();
    ensure!(
        matches!(unsafe { GetDriveTypeW(drive.as_ptr()) }, 2 | 3 | 6),
        "Copied image must be on a local drive"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CLIPBOARD_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ClipboardImage::from_file_bytes(&bytes)
}

struct Clipboard(HWND);
impl Clipboard {
    fn read() -> Result<Self> {
        ensure!(
            unsafe { OpenClipboard(null_mut()) } != 0,
            "Image clipboard is busy"
        );
        Ok(Self(null_mut()))
    }
    fn write() -> Result<Self> {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        ensure!(!window.is_null(), "Create image clipboard owner failed");
        if unsafe { OpenClipboard(window) } == 0 {
            unsafe {
                DestroyWindow(window);
            }
            bail!("Image clipboard is busy");
        }
        Ok(Self(window))
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
            if !self.0.is_null() {
                DestroyWindow(self.0);
            }
        }
    }
}

fn registered(name: &str) -> Result<u32> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    ensure!(format != 0, "Register image clipboard format failed");
    Ok(format)
}

fn read_bytes(format: u32, limit: usize) -> Result<Vec<u8>> {
    let handle = unsafe { GetClipboardData(format) };
    ensure!(!handle.is_null(), "Read image clipboard format failed");
    let size = unsafe { GlobalSize(handle) };
    ensure!(
        size > 0 && size <= limit,
        "Image clipboard allocation exceeds its limit"
    );
    let pointer = unsafe { GlobalLock(handle) };
    ensure!(!pointer.is_null(), "Lock image clipboard format failed");
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec();
    unsafe {
        GlobalUnlock(handle);
    }
    Ok(bytes)
}

pub fn read_image() -> Result<Option<ClipboardImage>> {
    read_image_sample().map(|(_, image)| image)
}

/// Return the sequence associated with the locked clipboard bytes. Rendering a
/// delayed format can advance the sequence, so sample after copying, before
/// releasing the clipboard for the bounded decode work.
pub fn read_image_sample() -> Result<(u32, Option<ClipboardImage>)> {
    let png = registered("PNG")?;
    let png_mime = registered("image/png")?;
    let origin_format = registered(IMAGE_ORIGIN_FORMAT)?;
    let (sequence, source, origin) = {
        let _clipboard = Clipboard::read()?;
        // Prefer PNG so alpha and the canonical digest survive an echo. The OS
        // can synthesize CF_DIB from CF_BITMAP for applications with only a bitmap.
        let mut source = None;
        for format in [png, png_mime, CF_DIBV5, CF_DIB] {
            if unsafe { IsClipboardFormatAvailable(format) } != 0 {
                if let Ok(bytes) = read_bytes(
                    format,
                    if format == png || format == png_mime {
                        MAX_CLIPBOARD_IMAGE_BYTES
                    } else {
                        MAX_DIB_BYTES
                    },
                ) {
                    source = Some(Source::Pixels(format == png || format == png_mime, bytes));
                    break;
                }
            }
        }
        if source.is_none() && unsafe { IsClipboardFormatAvailable(CF_HDROP) } != 0 {
            source = image_drop_path(&read_bytes(CF_HDROP, 64 * 1024)?)?.map(Source::File);
        }
        let origin = if unsafe { IsClipboardFormatAvailable(origin_format) } != 0 {
            read_bytes(origin_format, 32)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        } else {
            None
        };
        (unsafe { GetClipboardSequenceNumber() }, source, origin)
    };
    let Some(source) = source else {
        return Ok((sequence, None));
    };
    let image = match source {
        Source::Pixels(true, bytes) => ClipboardImage::from_png(&bytes)?,
        Source::Pixels(false, bytes) => ClipboardImage::from_dib(&bytes)?,
        Source::File(path) => read_local_image(&path)?,
    };
    // A shell metadata update can advance the clipboard sequence without a
    // fresh Copy. Preserve received-image provenance for as long as the pixels
    // still match; a new normal Copy clears the marker with EmptyClipboard.
    Ok((sequence, external_image(image, origin)))
}

struct Memory(*mut core::ffi::c_void);
impl Memory {
    fn new(bytes: &[u8]) -> Result<Self> {
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        ensure!(!handle.is_null(), "Allocate image clipboard format failed");
        let memory = Self(handle);
        let pointer = unsafe { GlobalLock(handle) };
        ensure!(!pointer.is_null(), "Lock image clipboard allocation failed");
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
            GlobalUnlock(handle);
        }
        Ok(memory)
    }
    fn publish(&mut self, format: u32) -> Result<()> {
        ensure!(
            !unsafe { SetClipboardData(format, self.0) }.is_null(),
            "Publish image clipboard format failed"
        );
        self.0 = null_mut();
        Ok(())
    }
}
impl Drop for Memory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                GlobalFree(self.0);
            }
        }
    }
}

fn current_references(cache: &Path) -> Result<Vec<PathBuf>> {
    if unsafe { IsClipboardFormatAvailable(CF_HDROP) } == 0 {
        return Ok(vec![]);
    }
    let bytes = read_bytes(CF_HDROP, 64 * 1024)?;
    ensure!(
        bytes.len() >= 20 && read_u32(&bytes, 16)? == 1,
        "Unsupported file clipboard format"
    );
    let offset = read_u32(&bytes, 0)? as usize;
    ensure!(
        offset >= 20 && offset <= bytes.len() && (bytes.len() - offset).is_multiple_of(2),
        "Invalid file clipboard format"
    );
    let units: Vec<u16> = bytes[offset..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    ensure!(
        units.ends_with(&[0, 0]),
        "Unterminated file clipboard format"
    );
    let mut paths = vec![];
    for part in units
        .split(|unit| *unit == 0)
        .filter(|part| !part.is_empty())
    {
        let path = PathBuf::from(String::from_utf16(part)?);
        // Inspect names only. Never open a source clipboard path, and retain
        // only references to our known cache directory for eviction exclusion.
        if path.parent().is_some_and(|parent| {
            parent
                .to_string_lossy()
                .eq_ignore_ascii_case(&cache.to_string_lossy())
        }) && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(managed_name)
        {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn file_drop(path: &Path) -> Result<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    ensure!(path.is_absolute(), "Image cache path is not absolute");
    let units: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut bytes = vec![0; 20]; // DROPFILES: DWORD, POINT, BOOL, BOOL (all 32-bit).
    put_u32(&mut bytes, 0, 20);
    put_u32(&mut bytes, 16, 1);
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(bytes)
}

fn mark(path: &Path) -> Result<()> {
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    // The cache contains app-generated images only. Reusing a verified managed
    // PNG reasserts ZoneId=3 rather than trusting old metadata or a peer path.
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(PathBuf::from(stream))?;
    file.write_all(b"[ZoneTransfer]\r\nZoneId=3\r\n")?;
    file.sync_all()?;
    Ok(())
}

/// All allocation, inspection and disk work happens before taking the engine
/// state lock. The guards stay on this worker until after the guarded publish.
pub struct PreparedPublication {
    prepared: PreparedImage,
    plan: CachePlan,
    png_format: u32,
    preferred_effect: u32,
    origin_format: u32,
    origin: Memory,
    png: Memory,
    dib_v5: Memory,
    dib: Memory,
    drop: Memory,
    copy_effect: Memory,
    rename_from: Vec<u16>,
    rename_to: Vec<u16>,
    _clipboard: Clipboard,
    _cache: MutexGuard<'static, ()>,
}

pub fn prepare_publication(prepared: PreparedImage) -> Result<PreparedPublication> {
    use std::os::windows::ffi::OsStrExt;
    let cache = CACHE_COMMIT
        .try_lock()
        .map_err(|_| anyhow::anyhow!("Image clipboard cache is busy"))?;
    let png_format = registered("PNG")?;
    let preferred_effect = registered("Preferred DropEffect")?;
    let origin_format = registered(IMAGE_ORIGIN_FORMAT)?;
    let origin = Memory::new(&prepared.digest())?;
    // Allocate before opening the clipboard or changing its current formats.
    let png = Memory::new(&prepared.png)?;
    let dib_v5 = Memory::new(&prepared.dib_v5)?;
    let dib = Memory::new(&prepared.dib)?;
    let copy_effect = Memory::new(&1u32.to_le_bytes())?; // DROPEFFECT_COPY only.
    let clipboard = Clipboard::write()?;
    let referenced = current_references(&prepared.cache)?;
    let plan = prepare_cache(&prepared, &referenced, mark)?;
    let drop = Memory::new(&file_drop(&plan.destination)?)?;
    let rename_from = prepared
        .temporary
        .as_ref()
        .context("Missing clipboard image temporary file")?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let rename_to = plan
        .destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    Ok(PreparedPublication {
        prepared,
        plan,
        png_format,
        preferred_effect,
        origin_format,
        origin,
        png,
        dib_v5,
        dib,
        drop,
        copy_effect,
        rename_from,
        rename_to,
        _clipboard: clipboard,
        _cache: cache,
    })
}

impl PreparedPublication {
    /// The engine calls this only while its original session/policy is current.
    /// Allocations and destructors remain outside that short commit scope.
    pub fn publish(&mut self) -> Result<()> {
        finish_cache(&mut self.prepared, &self.plan, |_, _| {
            // No REPLACE_EXISTING: a racing cache path never overwrites a file.
            ensure!(
                unsafe { MoveFileExW(self.rename_from.as_ptr(), self.rename_to.as_ptr(), 0) } != 0,
                "Commit clipboard image cache failed"
            );
            Ok(())
        })?;
        ensure!(
            unsafe { EmptyClipboard() } != 0,
            "Empty image clipboard failed"
        );
        self.origin.publish(self.origin_format)?;
        self.png.publish(self.png_format)?;
        self.dib_v5.publish(CF_DIBV5)?;
        self.dib.publish(CF_DIB)?;
        self.drop.publish(CF_HDROP)?;
        self.copy_effect.publish(self.preferred_effect)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_import_reads_owned_png_fixture_without_opening_clipboard() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("emc-image-import-{}-{suffix}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_file(self.0.join("image.png"));
                let _ = fs::remove_dir(&self.0);
            }
        }
        let _fixture = Fixture(directory.clone());
        let path = directory.join("image.png");
        let expected = ClipboardImage::new(1, 1, vec![1, 2, 3, 255]).unwrap();
        fs::write(&path, expected.to_png().unwrap()).unwrap();
        assert_eq!(read_local_image(&path).unwrap(), expected);
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_CLIPBOARD_IMAGE_BYTES as u64 + 1).unwrap();
        drop(file);
        assert!(read_local_image(&path).is_err());
        assert!(read_local_image(&directory).is_err());
        assert!(read_local_image(Path::new(r"\\server\share\image.png")).is_err());
    }
}
