use crate::wire::Value;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, Metadata},
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Stamp {
    pub kind: &'static str,
    pub size: i64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub id: Vec<u8>,
    pub volume: Vec<u8>,
}
impl Stamp {
    pub fn wire(&self) -> Value {
        Value::map([
            ("size", self.size.into()),
            ("mtime_ns", self.mtime_ns.into()),
            ("ctime_ns", self.ctime_ns.into()),
            ("ino", self.id.clone().into()),
        ])
    }
}
pub fn alias(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}
pub fn stamp(path: &Path) -> io::Result<Stamp> {
    let m = fs::symlink_metadata(path)?;
    let kind = if alias(&m) {
        "other"
    } else if m.is_dir() {
        "dir"
    } else if m.is_file() {
        "file"
    } else {
        "other"
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Stamp {
            kind,
            size: i64::try_from(m.len()).map_err(io::Error::other)?,
            mtime_ns: m
                .mtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(m.mtime_nsec()),
            ctime_ns: m
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(m.ctime_nsec()),
            id: m.ino().to_be_bytes().to_vec(),
            volume: m.dev().to_be_bytes().to_vec(),
        })
    }
    #[cfg(windows)]
    {
        use std::{
            mem::{size_of, zeroed},
            os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
        };
        use windows_sys::Win32::Storage::FileSystem::*;
        let f = fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut basic: FILE_BASIC_INFO = unsafe { zeroed() };
        let mut id: FILE_ID_INFO = unsafe { zeroed() };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                f.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let ok = unsafe {
            GetFileInformationByHandleEx(
                f.as_raw_handle(),
                FileIdInfo,
                (&mut id as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Stamp {
            kind,
            size: i64::try_from(m.len()).map_err(io::Error::other)?,
            mtime_ns: windows_ns(basic.LastWriteTime),
            ctime_ns: windows_ns(basic.ChangeTime),
            id: id.FileId.Identifier.to_vec(),
            volume: id.VolumeSerialNumber.to_be_bytes().to_vec(),
        })
    }
}
type Listing = BTreeMap<Vec<u8>, io::Result<Stamp>>;

#[cfg(unix)]
pub fn listing(path: &Path) -> io::Result<Listing> {
    let mut listing = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        listing.insert(encode(&entry.file_name()), stamp(&entry.path()));
    }
    Ok(listing)
}

#[cfg(windows)]
fn windows_ns(value: i64) -> i64 {
    ((value as i128 - 116_444_736_000_000_000) * 100).clamp(i64::MIN as i128, i64::MAX as i128)
        as i64
}

#[cfg(windows)]
pub fn listing(path: &Path) -> io::Result<Listing> {
    use std::{
        mem::{size_of, zeroed},
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };
    use windows_sys::Win32::{Foundation::ERROR_NO_MORE_FILES, Storage::FileSystem::*};

    // One directory handle supplies names, size, precise ChangeTime and the
    // same 128-bit IDs used by stamp(). No per-child opens are required.
    let directory = fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut identity: FILE_ID_INFO = unsafe { zeroed() };
    if unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            FileIdInfo,
            (&mut identity as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let volume = identity.VolumeSerialNumber.to_be_bytes();
    // u64 storage satisfies the API's 8-byte alignment requirement.
    let mut buffer = vec![0u64; 8192];
    let mut listing = BTreeMap::new();
    loop {
        buffer.fill(0);
        if unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                FileIdExtdDirectoryInfo,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u64>()) as u32,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                return Ok(listing);
            }
            // A partial/unsupported enumeration must not prove any deletion.
            return Err(error);
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>(),
                buffer.len() * size_of::<u64>(),
            )
        };
        read_windows_batch(bytes, &volume, &mut listing)?;
    }
}

#[cfg(windows)]
fn read_windows_batch(bytes: &[u8], volume: &[u8], listing: &mut Listing) -> io::Result<()> {
    use std::mem::{offset_of, size_of};
    use windows_sys::Win32::Storage::FileSystem::*;

    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid directory metadata batch",
        )
    };
    let mut offset = 0;
    loop {
        let remaining = bytes.get(offset..).ok_or_else(invalid)?;
        if remaining.len() < size_of::<FILE_ID_EXTD_DIR_INFO>() {
            return Err(invalid());
        }
        // The kernel writes plain integer fields; unaligned reads also make
        // the parser safe to exercise with synthetic byte buffers in tests.
        let info = unsafe {
            remaining
                .as_ptr()
                .cast::<FILE_ID_EXTD_DIR_INFO>()
                .read_unaligned()
        };
        let start = offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
        let length = info.FileNameLength as usize;
        if length == 0 || length % 2 != 0 || info.EndOfFile < 0 {
            return Err(invalid());
        }
        let end = start.checked_add(length).ok_or_else(invalid)?;
        let name = remaining.get(start..end).ok_or_else(invalid)?;
        let name: Vec<u16> = name
            .chunks_exact(2)
            .map(|n| u16::from_le_bytes([n[0], n[1]]))
            .collect();
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            let kind = if info.FileAttributes
                & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE)
                != 0
            {
                "other"
            } else if info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                "dir"
            } else {
                "file"
            };
            listing.insert(
                encode_wide(&name),
                Ok(Stamp {
                    kind,
                    size: info.EndOfFile,
                    mtime_ns: windows_ns(info.LastWriteTime),
                    ctime_ns: windows_ns(info.ChangeTime),
                    id: info.FileId.Identifier.to_vec(),
                    volume: volume.to_vec(),
                }),
            );
        }
        let next = info.NextEntryOffset as usize;
        if next == 0 {
            return Ok(());
        }
        if next < end || next >= remaining.len() || next % 8 != 0 {
            return Err(invalid());
        }
        offset += next;
    }
}
#[cfg(unix)]
pub fn encode(path: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_bytes().to_vec()
}
#[cfg(unix)]
pub fn decode(bytes: &[u8]) -> io::Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()))
}
#[cfg(windows)]
pub fn encode(path: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<_> = path.encode_wide().collect();
    encode_wide(&wide)
}
#[cfg(any(windows, test))]
fn encode_wide(wide: &[u16]) -> Vec<u8> {
    let mut out = vec![];
    let mut i = 0;
    while i < wide.len() {
        let mut c = wide[i] as u32;
        i += 1;
        if (0xd800..=0xdbff).contains(&c) && i < wide.len() && (0xdc00..=0xdfff).contains(&wide[i])
        {
            c = 0x10000 + ((c - 0xd800) << 10) + (wide[i] as u32 - 0xdc00);
            i += 1;
        }
        if c < 0x80 {
            out.push(c as u8);
        } else if c < 0x800 {
            out.extend([(0xc0 | (c >> 6)) as u8, (0x80 | (c & 63)) as u8]);
        } else if c < 0x10000 {
            out.extend([
                (0xe0 | (c >> 12)) as u8,
                (0x80 | ((c >> 6) & 63)) as u8,
                (0x80 | (c & 63)) as u8,
            ]);
        } else {
            out.extend([
                (0xf0 | (c >> 18)) as u8,
                (0x80 | ((c >> 12) & 63)) as u8,
                (0x80 | ((c >> 6) & 63)) as u8,
                (0x80 | (c & 63)) as u8,
            ]);
        }
    }
    out
}
#[cfg(any(windows, test))]
fn decode_wide(bytes: &[u8]) -> io::Result<Vec<u16>> {
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        let (mut c, n, min) = if b < 128 {
            (b as u32, 0, 0)
        } else if b & 0xe0 == 0xc0 {
            ((b & 31) as u32, 1, 0x80)
        } else if b & 0xf0 == 0xe0 {
            ((b & 15) as u32, 2, 0x800)
        } else if b & 0xf8 == 0xf0 {
            ((b & 7) as u32, 3, 0x10000)
        } else {
            return Err(io::Error::other("invalid WTF-8"));
        };
        for _ in 0..n {
            let b = *bytes
                .get(i)
                .ok_or_else(|| io::Error::other("invalid WTF-8"))?;
            i += 1;
            if b & 0xc0 != 0x80 {
                return Err(io::Error::other("invalid WTF-8"));
            }
            c = (c << 6) | (b & 63) as u32;
        }
        if c < min || c > 0x10ffff {
            return Err(io::Error::other("invalid WTF-8"));
        }
        if c < 0x10000 {
            out.push(c as u16);
        } else {
            let c = c - 0x10000;
            out.extend([(0xd800 + (c >> 10)) as u16, (0xdc00 + (c & 1023)) as u16]);
        }
    }
    Ok(out)
}
#[cfg(windows)]
pub fn decode(bytes: &[u8]) -> io::Result<OsString> {
    use std::os::windows::ffi::OsStringExt;
    Ok(OsString::from_wide(&decode_wide(bytes)?))
}

pub fn relative(path: &Path) -> io::Result<Vec<u8>> {
    let mut out = vec![];
    for c in path.components() {
        if let std::path::Component::Normal(name) = c {
            if !out.is_empty() {
                out.push(b'/');
            }
            out.extend(encode(name));
        } else {
            return Err(io::Error::other("path is not relative"));
        }
    }
    Ok(out)
}
pub fn resolve(root: &Path, relative: &[u8]) -> io::Result<PathBuf> {
    let mut path = root.to_path_buf();
    if relative.is_empty() {
        return Ok(path);
    }
    for part in relative.split(|b| *b == b'/') {
        if part.is_empty() || part == b"." || part == b".." || part.contains(&0) {
            return Err(io::Error::other("invalid relative path"));
        }
        #[cfg(windows)]
        if part.contains(&b'\\') || part.contains(&b':') {
            return Err(io::Error::other("invalid Windows component"));
        }
        path.push(decode(part)?);
        match fs::symlink_metadata(&path) {
            Ok(m) if alias(&m) => {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "path alias"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(path)
}
pub fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    if parent.is_empty() {
        name.to_vec()
    } else {
        [parent, b"/", name].concat()
    }
}
pub fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn bulk_directory_metadata_matches_path_inspection_across_batches() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..1600 {
            fs::write(
                root.path()
                    .join(format!("{i:04}-a-long-directory-entry.txt")),
                b"data",
            )
            .unwrap();
        }
        fs::create_dir(root.path().join("folder")).unwrap();
        let raw_name = encode_wide(&[0x61, 0xd800, 0x62]);
        fs::write(root.path().join(decode(&raw_name).unwrap()), b"raw").unwrap();
        let entries = listing(root.path()).unwrap();
        assert_eq!(entries.len(), 1602);
        assert!(entries.contains_key(&raw_name));
        for (name, observed) in entries {
            let inspected = stamp(&root.path().join(decode(&name).unwrap())).unwrap();
            assert_eq!(
                observed.unwrap(),
                inspected,
                "metadata mismatch for {name:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn bulk_listing_includes_exclusively_open_files() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("exclusive");
        fs::write(&path, b"data").unwrap();
        let expected = stamp(&path).unwrap();
        let _exclusive = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let mut entries = listing(root.path()).unwrap();
        assert_eq!(
            entries.remove(b"exclusive".as_slice()).unwrap().unwrap(),
            expected
        );
    }

    #[cfg(windows)]
    #[test]
    fn malformed_bulk_metadata_rejects_the_listing() {
        use std::mem::{offset_of, size_of};
        use windows_sys::Win32::Storage::FileSystem::FILE_ID_EXTD_DIR_INFO;
        let mut bytes = vec![0u8; size_of::<FILE_ID_EXTD_DIR_INFO>()];
        let length = offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength);
        bytes[length..length + 4].copy_from_slice(&3u32.to_le_bytes());
        assert!(read_windows_batch(&bytes, &[0; 8], &mut BTreeMap::new()).is_err());
    }

    #[test]
    fn wtf8_round_trip() {
        let v = [0x61, 0xd800, 0x62, 0xdc00, 0xd83d, 0xde00];
        assert_eq!(decode_wide(&encode_wide(&v)).unwrap(), v);
        assert!(decode_wide(&[0xc0, 0x80]).is_err());
    }
    #[test]
    fn rejects_parent_paths() {
        assert!(resolve(Path::new("."), b"../outside").is_err());
    }
}
