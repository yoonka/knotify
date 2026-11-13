//! Lossless relative path encoding for watcher hints.
use std::{ffi::OsStr, io, path::Path};
#[cfg(unix)]
pub fn encode(path: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_bytes().to_vec()
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
#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wtf8_round_trip() {
        let wide = [0x61, 0xd800, 0xdc00, 0xd83d, 0xde00];
        assert_eq!(decode_wide(&encode_wide(&wide)).unwrap(), wide);
        assert!(decode_wide(&[0xc0, 0x80]).is_err());
    }
    #[test]
    fn paths_stay_relative() {
        assert!(relative(Path::new("../outside")).is_err());
    }
}
