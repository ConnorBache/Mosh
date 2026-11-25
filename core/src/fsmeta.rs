use crate::millis;
use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Default)]
pub(crate) struct Native {
    pub id: Option<String>,
    pub volume: Option<String>,
    pub filesystem: Option<String>,
    pub allocated: Option<i64>,
    pub changed: Option<i64>,
    pub hardlinks: Option<i64>,
    pub attributes: Option<u32>,
}

#[cfg(windows)]
pub(crate) fn reparse(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
pub(crate) fn reparse(meta: &Metadata) -> bool {
    meta.is_symlink()
}

#[cfg(windows)]
pub(crate) mod win {
    use super::*;
    use std::{
        mem::{self, MaybeUninit},
        os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
    };
    use windows_sys::Win32::{Foundation::HANDLE, Storage::FileSystem::*};

    fn attr_file(path: &Path, access: u32) -> std::io::Result<File> {
        File::options()
            .access_mode(access)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    fn read_info<T>(handle: HANDLE, class: FILE_INFO_BY_HANDLE_CLASS) -> Option<T> {
        let mut result = MaybeUninit::<T>::uninit();
        // Each call pairs the Windows information class with its documented buffer type.
        unsafe {
            (GetFileInformationByHandleEx(
                handle,
                class,
                result.as_mut_ptr().cast(),
                mem::size_of::<T>() as u32,
            ) != 0)
                .then(|| result.assume_init())
        }
    }

    pub(super) fn read_native(path: &Path, meta: &Metadata) -> Native {
        let mut result = Native {
            attributes: Some(meta.file_attributes()),
            ..Default::default()
        };
        let Ok(file) = attr_file(path, 0) else {
            return result;
        };
        let handle = file.as_raw_handle() as HANDLE;
        if let Some(id) = read_info::<FILE_ID_INFO>(handle, FileIdInfo) {
            let key = u128::from_le_bytes(id.FileId.Identifier);
            if key != 0 {
                result.id = Some(format!("{:016x}:{key:032x}", id.VolumeSerialNumber));
                result.volume = Some(format!("{:016x}", id.VolumeSerialNumber));
            }
        }
        if let Some(info) = read_info::<FILE_STANDARD_INFO>(handle, FileStandardInfo) {
            result.allocated = Some(info.AllocationSize);
            result.hardlinks = Some(i64::from(info.NumberOfLinks));
        }
        if let Some(info) = read_info::<FILE_BASIC_INFO>(handle, FileBasicInfo) {
            result.changed = info
                .ChangeTime
                .checked_div(10_000)
                .and_then(|ms| ms.checked_sub(11_644_473_600_000));
        }
        result
    }

    pub(crate) fn filesystem(path: &Path) -> Option<String> {
        let file = attr_file(path, 0).ok()?;
        let mut fs_name = [0u16; 64];
        // Optional output buffers can be null when only the filesystem name is needed.
        let success = unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle().cast(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        (success != 0).then(|| {
            String::from_utf16_lossy(
                &fs_name[..fs_name
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(fs_name.len())],
            )
        })
    }

    pub(super) fn read_security(path: &Path) -> serde_json::Value {
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{Authorization::*, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION},
        };
        let mut descriptor = std::ptr::null_mut();
        let Ok(file) = attr_file(path, 0x20000) else {
            return serde_json::Value::Null;
        };
        // READ_CONTROL requests permission to read the owner and discretionary ACL.
        unsafe {
            let flags = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            if GetSecurityInfo(
                file.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            ) != 0
            {
                return serde_json::Value::Null;
            }
            let mut text = std::ptr::null_mut();
            let mut len = 0;
            let status = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor, 1, flags, &mut text, &mut len,
            );
            let sddl = (status != 0 && !text.is_null()).then(|| {
                String::from_utf16_lossy(std::slice::from_raw_parts(text, len as usize))
                    .trim_end_matches('\0')
                    .to_owned()
            });
            if !text.is_null() {
                LocalFree(text.cast());
            }
            LocalFree(descriptor.cast());
            serde_json::json!({"sddl":sddl})
        }
    }

    pub(crate) fn open_snapshot(path: &Path) -> std::io::Result<File> {
        // Share reads only while hashing to prevent writes, replacement, or removal.
        File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
}

#[cfg(windows)]
pub(crate) fn native(path: &Path, meta: &Metadata) -> Native {
    win::read_native(path, meta)
}
#[cfg(windows)]
pub(crate) fn security(path: &Path) -> serde_json::Value {
    win::read_security(path)
}
#[cfg(not(windows))]
pub(crate) fn security(_path: &Path) -> serde_json::Value {
    serde_json::Value::Null
}

#[cfg(unix)]
pub(crate) fn native(_path: &Path, meta: &Metadata) -> Native {
    use std::os::unix::fs::MetadataExt;
    Native {
        id: Some(format!("{}:{}", meta.dev(), meta.ino())),
        volume: Some(meta.dev().to_string()),
        allocated: meta
            .blocks()
            .checked_mul(512)
            .and_then(|n| i64::try_from(n).ok()),
        changed: meta
            .ctime()
            .checked_mul(1000)
            .map(|n| n + meta.ctime_nsec() / 1_000_000),
        hardlinks: i64::try_from(meta.nlink()).ok(),
        attributes: Some(meta.mode()),
        filesystem: None,
    }
}

pub(crate) fn revision_stamp(meta: &Metadata) -> (u64, Option<i64>, Option<i64>) {
    (
        meta.len(),
        meta.modified().ok().map(millis),
        meta.created().ok().map(millis),
    )
}

pub(crate) fn open_snapshot(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        win::open_snapshot(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

pub(crate) fn text(path: &Path, extension: &str) -> String {
    inspect_text(path, extension).unwrap_or_default()
}

const TEXT_LIMIT: u64 = 128 * 1024;

fn inspect_text(path: &Path, extension: &str) -> anyhow::Result<String> {
    let file = open_snapshot(path)?;
    if matches!(
        extension,
        "docx" | "pptx" | "xlsx" | "odt" | "ods" | "odp" | "zip"
    ) {
        // Cap archive size before parsing its central directory and bound decompression.
        if file.metadata()?.len() > 32 * 1024 * 1024 {
            return Ok(String::new());
        }
        let mut zip = zip::ZipArchive::new(file)?;
        let mut result = String::new();
        let mut remaining = TEXT_LIMIT;
        for i in 0..zip.len().min(512) {
            let mut entry = zip.by_index(i)?;
            let name = entry.name().to_owned();
            if extension == "zip" {
                if let Some(name) = entry
                    .enclosed_name()
                    .and_then(|x| x.to_str().map(str::to_owned))
                {
                    if result.len() + name.len() + 1 > TEXT_LIMIT as usize {
                        break;
                    }
                    result.push_str(&name);
                    result.push('\n');
                }
                continue;
            }
            if !matches!(
                name.as_str(),
                "word/document.xml" | "xl/sharedStrings.xml" | "content.xml"
            ) && !(name.starts_with("ppt/slides/slide") && name.ends_with(".xml"))
                && !(name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml"))
            {
                continue;
            }
            if remaining == 0 {
                break;
            }
            let mut xml = Vec::new();
            (&mut entry).take(remaining).read_to_end(&mut xml)?;
            remaining -= xml.len() as u64;
            let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
            loop {
                match reader.read_event() {
                    Ok(quick_xml::events::Event::Text(chunk)) => {
                        let decoded = chunk.xml_content()?;
                        result.push_str(&decoded);
                        result.push(' ');
                    }
                    Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                    _ => {}
                }
            }
        }
        return Ok(result);
    }
    if !matches!(
        extension,
        "txt"
            | "md"
            | "markdown"
            | "rs"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "py"
            | "cs"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "java"
            | "kt"
            | "go"
            | "rb"
            | "swift"
            | "html"
            | "css"
            | "sh"
            | "ps1"
            | "bat"
            | "cmd"
            | "lua"
            | "sql"
            | "json"
            | "jsonl"
            | "xml"
            | "yaml"
            | "yml"
            | "toml"
            | "csv"
            | "tsv"
            | "ini"
            | "log"
            | "rst"
            | "svg"
            | "conf"
    ) {
        return Ok(String::new());
    }
    let mut data = Vec::new();
    file.take(TEXT_LIMIT).read_to_end(&mut data)?;
    if data.starts_with(&[0xff, 0xfe]) || data.starts_with(&[0xfe, 0xff]) {
        let little = data[0] == 0xff;
        let units: Vec<_> = data[2..]
            .chunks_exact(2)
            .map(|pair| {
                if little {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        return Ok(String::from_utf16_lossy(&units));
    }
    if data.contains(&0) {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&data)
        .trim_start_matches('\u{feff}')
        .to_owned())
}

pub(crate) fn partial(file: &mut File, size: u64) -> std::io::Result<String> {
    const BLOCK: u64 = 8192;
    let mut hash = blake3::Hasher::new();
    let mut block = [0; BLOCK as usize];
    for offset in [
        0,
        size.saturating_sub(BLOCK) / 2,
        size.saturating_sub(BLOCK),
    ] {
        file.seek(SeekFrom::Start(offset))?;
        let length = (size - offset).min(BLOCK) as usize;
        file.read_exact(&mut block[..length])?;
        hash.update(&block[..length]);
    }
    Ok(hash.finalize().to_hex().to_string())
}

pub(crate) fn hash(file: &mut File) -> std::io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = blake3::Hasher::new();
    std::io::copy(file, &mut hash)?;
    Ok(hash.finalize().to_hex().to_string())
}

pub(crate) fn hard_stamp(
    path: &Path,
) -> anyhow::Result<(u64, Option<i64>, Option<i64>, Option<String>)> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        !reparse(&metadata),
        "reparse points cannot be changed by this operation"
    );
    let (len, modification, creation) = revision_stamp(&metadata);
    Ok((len, modification, creation, native(path, &metadata).id))
}
