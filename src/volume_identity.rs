//! Supplement dev/inode identity with a filesystem UUID or ZFS dataset GUID
//! when exposed to the ordinary user. Never run a shell or require privilege.
#[cfg(target_os = "linux")]
use std::fs;
use std::{io, path::Path};

#[cfg(target_os = "linux")]
pub fn read(root: &Path) -> io::Result<Option<Vec<u8>>> {
    let mounts = fs::read("/proc/self/mountinfo")?;
    let mut selected = None;
    for line in mounts.split(|b| *b == b'\n') {
        let fields: Vec<_> = line.split(|b| *b == b' ').collect();
        let Some(separator) = fields.iter().position(|p| *p == b"-") else {
            continue;
        };
        if fields.len() <= separator + 2 || fields.len() < 5 {
            continue;
        }
        let mount = crate::native::decode(&unescape(fields[4]))?;
        if root.starts_with(Path::new(&mount))
            && selected
                .as_ref()
                .is_none_or(|(len, _, _)| *len < fields[4].len())
        {
            selected = Some((
                fields[4].len(),
                fields[separator + 1],
                unescape(fields[separator + 2]),
            ));
        }
    }
    let Some((_, kind, source)) = selected else {
        return Ok(None);
    };
    if kind == b"zfs" {
        return Ok(zfs_guid(&source));
    }
    let source = Path::new(&crate::native::decode(&source)?).to_path_buf();
    let Ok(source) = fs::canonicalize(source) else {
        return Ok(None);
    };
    let Ok(entries) = fs::read_dir("/dev/disk/by-uuid") else {
        return Ok(None);
    };
    for entry in entries {
        let entry = entry?;
        if fs::canonicalize(entry.path()).is_ok_and(|p| p == source) {
            return Ok(Some(
                [
                    b"uuid:".as_slice(),
                    &crate::native::encode(&entry.file_name()),
                ]
                .concat(),
            ));
        }
    }
    Ok(None)
}

#[cfg(target_os = "freebsd")]
pub fn read(root: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::ffi::{CStr, CString};
    let root = CString::new(crate::native::encode(root.as_os_str()))?;
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(root.as_ptr(), &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = unsafe { CStr::from_ptr(info.f_fstypename.as_ptr()) };
    let source = unsafe { CStr::from_ptr(info.f_mntfromname.as_ptr()) };
    if kind.to_bytes() == b"zfs" {
        Ok(zfs_guid(source.to_bytes()))
    } else {
        Ok(None)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub fn read(_root: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn zfs_guid(dataset: &[u8]) -> Option<Vec<u8>> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let program = ["/sbin/zfs", "/usr/sbin/zfs"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .unwrap_or("zfs");
    let mut child = Command::new(program)
        .args(["get", "-H", "-p", "-o", "value", "guid"])
        .arg(crate::native::decode(dataset).ok()?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut value = String::new();
    child
        .stdout
        .take()?
        .take(128)
        .read_to_string(&mut value)
        .ok()?;
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("zfs:{value}").into_bytes())
}

#[cfg(target_os = "linux")]
fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && bytes
                .get(i + 1..i + 4)
                .is_some_and(|s| s.iter().all(|b| (b'0'..=b'7').contains(b)))
        {
            let s = &bytes[i + 1..i + 4];
            out.push(((s[0] - b'0') << 6) | ((s[1] - b'0') << 3) | (s[2] - b'0'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn mount_paths_keep_spaces_backslashes_and_raw_bytes() {
        assert_eq!(super::unescape(b"/a\\040b\\134c/\xff"), b"/a b\\c/\xff");
        assert!(super::read(tempfile::tempdir().unwrap().path()).is_ok());
    }
}
