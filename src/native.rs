use crate::wire::Value;
use std::{
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
        let ns = |v: i64| -> i64 {
            ((v as i128 - 116_444_736_000_000_000) * 100).clamp(i64::MIN as i128, i64::MAX as i128)
                as i64
        };
        Ok(Stamp {
            kind,
            size: i64::try_from(m.len()).map_err(io::Error::other)?,
            mtime_ns: ns(basic.LastWriteTime),
            ctime_ns: ns(basic.ChangeTime),
            id: id.FileId.Identifier.to_vec(),
            volume: id.VolumeSerialNumber.to_be_bytes().to_vec(),
        })
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
