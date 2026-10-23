//! A bounded ETF subset: binary-keyed maps, lists, integers, booleans and nil.
//! Never accepts compressed terms, references, executable terms or arbitrary atoms.
use std::collections::BTreeMap;
use std::io::{self, Read, Write};

pub const VERSION: i64 = 1;
pub const MAX_PACKET: usize = 1024 * 1024;
pub const MAX_ITEMS: usize = 8192;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Binary(Vec<u8>),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Bool(bool),
    Nil,
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::Binary(v.as_bytes().to_vec())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::from(v.as_str())
    }
}
impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Self::Binary(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}
impl Value {
    pub fn get(&self, key: &str) -> io::Result<&Self> {
        match self {
            Self::Map(m) => m.get(key).ok_or_else(invalid),
            _ => None.ok_or_else(invalid),
        }
    }
    pub fn bytes(&self) -> io::Result<&[u8]> {
        if let Self::Binary(v) = self {
            Ok(v)
        } else {
            Err(invalid())
        }
    }
    pub fn int(&self) -> io::Result<i64> {
        if let Self::Int(v) = self {
            Ok(*v)
        } else {
            Err(invalid())
        }
    }
}
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid ETF protocol message")
}
pub fn message(kind: &str, fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut m = BTreeMap::from([
        ("version".into(), Value::Int(VERSION)),
        ("type".into(), kind.into()),
    ]);
    m.extend(fields.into_iter().map(|(k, v)| (k.into(), v)));
    Value::Map(m)
}
pub fn encode(value: &Value) -> io::Result<Vec<u8>> {
    let mut out = vec![131];
    encode_value(value, &mut out, 0)?;
    if out.len() > MAX_PACKET {
        return Err(invalid());
    }
    Ok(out)
}
fn encode_value(value: &Value, out: &mut Vec<u8>, depth: usize) -> io::Result<()> {
    if depth > 16 || out.len() > MAX_PACKET {
        return Err(invalid());
    }
    match value {
        Value::Int(n) if (0..=255).contains(n) => out.extend([97, *n as u8]),
        Value::Int(n) if i32::try_from(*n).is_ok() => {
            out.push(98);
            out.extend((*n as i32).to_be_bytes());
        }
        Value::Int(n) => {
            out.extend([110, 8, u8::from(*n < 0)]);
            out.extend(n.unsigned_abs().to_le_bytes());
        }
        Value::Binary(v) => {
            out.push(109);
            out.extend((v.len() as u32).to_be_bytes());
            out.extend(v);
        }
        Value::List(v) => {
            out.push(108);
            out.extend((v.len() as u32).to_be_bytes());
            for x in v {
                encode_value(x, out, depth + 1)?;
            }
            out.push(106);
        }
        Value::Map(m) => {
            out.push(116);
            out.extend((m.len() as u32).to_be_bytes());
            for (k, v) in m {
                encode_value(&Value::from(k.as_str()), out, depth + 1)?;
                encode_value(v, out, depth + 1)?;
            }
        }
        Value::Bool(b) => {
            let s = if *b { "true" } else { "false" };
            out.extend([119, s.len() as u8]);
            out.extend(s.as_bytes());
        }
        Value::Nil => out.extend([119, 3, b'n', b'i', b'l']),
    }
    Ok(())
}
pub fn decode(bytes: &[u8]) -> io::Result<Value> {
    decode_bounded(bytes, MAX_ITEMS)
}
fn decode_bounded(bytes: &[u8], budget: usize) -> io::Result<Value> {
    if bytes.len() > MAX_PACKET || bytes.first() != Some(&131) {
        return Err(invalid());
    }
    let mut decoder = Decoder {
        bytes,
        pos: 1,
        budget,
    };
    let value = decoder.value(0)?;
    if decoder.pos != bytes.len() {
        return Err(invalid());
    }
    Ok(value)
}
struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    budget: usize,
}
impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(invalid)?;
        let b = self.bytes.get(self.pos..end).ok_or_else(invalid)?;
        self.pos = end;
        Ok(b)
    }
    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn len(&mut self) -> io::Result<usize> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
    fn value(&mut self, depth: usize) -> io::Result<Value> {
        if depth > 16 || self.budget == 0 {
            return Err(invalid());
        }
        self.budget -= 1;
        Ok(match self.byte()? {
            97 => Value::Int(self.byte()? as i64),
            98 => Value::Int(i32::from_be_bytes(self.take(4)?.try_into().unwrap()) as i64),
            110 => {
                let n = self.byte()? as usize;
                let sign = self.byte()?;
                if n > 8 || sign > 1 {
                    return Err(invalid());
                }
                let mut b = [0; 8];
                b[..n].copy_from_slice(self.take(n)?);
                let magnitude = u64::from_le_bytes(b);
                let v = if sign == 1 {
                    -(magnitude as i128)
                } else {
                    magnitude as i128
                };
                Value::Int(i64::try_from(v).map_err(|_| invalid())?)
            }
            109 => {
                let n = self.len()?;
                Value::Binary(self.take(n)?.to_vec())
            }
            106 => Value::List(vec![]),
            108 => {
                let n = self.len()?;
                if n > self.budget {
                    return Err(invalid());
                }
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.value(depth + 1)?);
                }
                if self.byte()? != 106 {
                    return Err(invalid());
                }
                Value::List(v)
            }
            116 => {
                let n = self.len()?;
                if n > self.budget / 2 {
                    return Err(invalid());
                }
                let mut m = BTreeMap::new();
                for _ in 0..n {
                    let k = self.value(depth + 1)?;
                    let k = String::from_utf8(k.bytes()?.to_vec()).map_err(|_| invalid())?;
                    if m.insert(k, self.value(depth + 1)?).is_some() {
                        return Err(invalid());
                    }
                }
                Value::Map(m)
            }
            119 => {
                let n = self.byte()? as usize;
                match self.take(n)? {
                    b"true" => Value::Bool(true),
                    b"false" => Value::Bool(false),
                    b"nil" => Value::Nil,
                    _ => return Err(invalid()),
                }
            }
            _ => return Err(invalid()),
        })
    }
}
pub fn read(reader: &mut impl Read) -> io::Result<Option<Value>> {
    let mut header = [0; 4];
    match reader.read(&mut header[..1])? {
        0 => return Ok(None),
        _ => reader.read_exact(&mut header[1..])?,
    }
    let n = u32::from_be_bytes(header) as usize;
    if n == 0 || n > MAX_PACKET {
        return Err(invalid());
    }
    let mut b = vec![0; n];
    reader.read_exact(&mut b)?;
    decode(&b).map(Some)
}
pub fn write(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let b = encode(value)?;
    writer.write_all(&(b.len() as u32).to_be_bytes())?;
    writer.write_all(&b)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_reject_unsafe_terms() {
        let v = message(
            "test",
            [
                ("path", vec![0xff, 10, b'"'].into()),
                ("n", i64::MIN.into()),
                ("ok", Value::Bool(true)),
                ("paths", Value::List(vec![])),
            ],
        );
        assert_eq!(decode(&encode(&v).unwrap()).unwrap(), v);
        assert!(decode(&[131, 80, 0, 0, 0, 0]).is_err());
        assert!(decode(&[131, 119, 3, b'f', b'o', b'o']).is_err());
        let mut b = encode(&v).unwrap();
        b.push(0);
        assert!(decode(&b).is_err());
    }
}
