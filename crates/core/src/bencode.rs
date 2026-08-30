use thiserror::Error;

/// Bencode value type. Keys in Dict are byte strings; the Vec preserves insertion order,
/// and callers must ensure keys are sorted when encoding.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Vec<(Vec<u8>, Value)>),
}

impl Value {
    /// Look up a key in a Dict by byte slice. Returns None if the value is not a Dict
    /// or the key is not present.
    pub fn dict_get(&self, key: &[u8]) -> Option<&Value> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("unexpected end of input at position {0}")]
    UnexpectedEof(usize),

    #[error("invalid byte {0:#x} at position {1}")]
    InvalidByte(u8, usize),

    #[error("invalid integer at position {0}")]
    InvalidInt(usize),
}

/// Encode a `Value` into its bencode byte representation.
pub fn encode(v: &Value) -> Vec<u8> {
    match v {
        Value::Int(i) => {
            let mut out = Vec::with_capacity(3 + decimal_len(i.unsigned_abs() as usize) + 1);
            out.extend_from_slice(b"i");
            out.extend_from_slice(i.to_string().as_bytes());
            out.push(b'e');
            out
        }
        Value::Bytes(b) => {
            let mut out = Vec::with_capacity(b.len() + decimal_len(b.len()) + 1);
            out.extend_from_slice(b.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(b);
            out
        }
        Value::List(items) => {
            let mut out = Vec::new();
            out.push(b'l');
            for item in items {
                out.extend(encode(item));
            }
            out.push(b'e');
            out
        }
        Value::Dict(items) => {
            let mut sorted = items.clone();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = Vec::new();
            out.push(b'd');
            for (k, v) in &sorted {
                out.extend(encode_bytes(k));
                out.extend(encode(v));
            }
            out.push(b'e');
            out
        }
    }
}

fn encode_bytes(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() + decimal_len(b.len()) + 1);
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
    out
}

fn decimal_len(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut len = 0;
    while n > 0 {
        n /= 10;
        len += 1;
    }
    len
}

/// Decode a complete bencode value from the start of `data`.
pub fn decode(data: &[u8]) -> Result<Value, DecodeError> {
    if data.is_empty() {
        return Err(DecodeError::UnexpectedEof(0));
    }
    let (v, pos) = decode_at(data, 0)?;
    if pos != data.len() {
        return Err(DecodeError::InvalidByte(data[pos], pos));
    }
    Ok(v)
}

fn decode_at(data: &[u8], pos: usize) -> Result<(Value, usize), DecodeError> {
    let c = byte_at(data, pos)?;
    match c {
        b'i' => decode_int(data, pos),
        b'0'..=b'9' => decode_bytes(data, pos),
        b'l' => decode_list(data, pos),
        b'd' => decode_dict(data, pos),
        _ => Err(DecodeError::InvalidByte(c, pos)),
    }
}

fn byte_at(data: &[u8], pos: usize) -> Result<u8, DecodeError> {
    data.get(pos).copied().ok_or(DecodeError::UnexpectedEof(pos))
}

fn decode_int(data: &[u8], pos: usize) -> Result<(Value, usize), DecodeError> {
    let start = pos + 1;
    let mut end = start;
    while end < data.len() {
        let c = data[end];
        if c == b'e' {
            break;
        }
        // allow optional leading minus
        if end == start && c == b'-' {
            end += 1;
            continue;
        }
        if !c.is_ascii_digit() {
            return Err(DecodeError::InvalidByte(c, end));
        }
        end += 1;
    }
    if end >= data.len() {
        return Err(DecodeError::UnexpectedEof(pos));
    }
    let num_str = &data[start..end];
    // Reject empty integers like "ie"
    if num_str.is_empty()
        || num_str == b"-"
        || (num_str.len() > 1 && num_str.starts_with(b"0"))
        || (num_str.starts_with(b"-0") && num_str.len() > 2)
    {
        return Err(DecodeError::InvalidInt(pos));
    }
    let num: i64 = std::str::from_utf8(num_str)
        .map_err(|_| DecodeError::InvalidInt(pos))?
        .parse()
        .map_err(|_| DecodeError::InvalidInt(pos))?;
    Ok((Value::Int(num), end + 1))
}

fn decode_bytes(data: &[u8], pos: usize) -> Result<(Value, usize), DecodeError> {
    let colon = data[pos..]
        .iter()
        .position(|&b| b == b':')
        .map(|p| p + pos)
        .ok_or(DecodeError::InvalidByte(b':', pos))?;
    let len_str = &data[pos..colon];
    let len: usize = std::str::from_utf8(len_str)
        .map_err(|_| DecodeError::InvalidByte(len_str[0], pos))?
        .parse()
        .map_err(|_| DecodeError::InvalidByte(len_str[0], pos))?;
    let start = colon + 1;
    if start + len > data.len() {
        return Err(DecodeError::UnexpectedEof(colon));
    }
    Ok((Value::Bytes(data[start..start + len].to_vec()), start + len))
}

fn decode_list(data: &[u8], pos: usize) -> Result<(Value, usize), DecodeError> {
    let mut items = Vec::new();
    let mut pos = pos + 1;
    while pos < data.len() && data[pos] != b'e' {
        let (v, next) = decode_at(data, pos)?;
        items.push(v);
        pos = next;
    }
    if pos >= data.len() {
        return Err(DecodeError::UnexpectedEof(pos));
    }
    Ok((Value::List(items), pos + 1))
}

fn decode_dict(data: &[u8], pos: usize) -> Result<(Value, usize), DecodeError> {
    let mut items = Vec::new();
    let mut pos = pos + 1;
    while pos < data.len() && data[pos] != b'e' {
        let (k, next) = decode_at(data, pos)?;
        let k = match k {
            Value::Bytes(b) => b,
            _ => return Err(DecodeError::InvalidByte(b'd', pos)),
        };
        pos = next;
        if pos >= data.len() {
            return Err(DecodeError::UnexpectedEof(pos));
        }
        let (v, next) = decode_at(data, pos)?;
        items.push((k, v));
        pos = next;
    }
    if pos >= data.len() {
        return Err(DecodeError::UnexpectedEof(pos));
    }
    Ok((Value::Dict(items), pos + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn encode_int() {
        assert_eq!(encode(&Value::Int(42)), b"i42e");
        assert_eq!(encode(&Value::Int(-5)), b"i-5e");
    }

    #[test]
    fn encode_bytes() {
        assert_eq!(encode(&Value::Bytes(b("spam"))), b"4:spam");
    }

    #[test]
    fn encode_list() {
        let v = Value::List(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(encode(&v), b"li1ei2ee");
    }

    #[test]
    fn encode_dict_sorts_keys() {
        let v = Value::Dict(vec![
            (b("b"), Value::Int(2)),
            (b("a"), Value::Int(1)),
        ]);
        assert_eq!(encode(&v), b"d1:ai1e1:bi2ee");
    }

    #[test]
    fn decode_int() {
        assert_eq!(decode(b"i42e").unwrap(), Value::Int(42));
    }

    #[test]
    fn decode_string() {
        assert_eq!(decode(b"4:spam").unwrap(), Value::Bytes(b("spam")));
    }

    #[test]
    fn decode_list() {
        assert_eq!(
            decode(b"li1ei2ee").unwrap(),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn decode_dict() {
        assert_eq!(
            decode(b"d3:foo3:bare").unwrap(),
            Value::Dict(vec![(b("foo"), Value::Bytes(b("bar")))])
        );
    }

    #[test]
    fn roundtrip_nested() {
        let v = Value::Dict(vec![
            (b("e"), Value::Int(0)),
            (
                b("m"),
                Value::Dict(vec![(b("ut_pex"), Value::Int(1)),]),
            ),
            (b("v"), Value::Bytes(b("XunLei 0019"))),
        ]);
        let encoded = encode(&v);
        assert_eq!(decode(&encoded).unwrap(), v);
    }

    #[test]
    fn decode_empty_input_errors() {
        assert!(decode(b"").is_err());
    }

    #[test]
    fn decode_truncated_errors() {
        assert!(decode(b"i42").is_err());
        assert!(decode(b"4:spa").is_err());
    }

    #[test]
    fn dict_get_works() {
        let d = Value::Dict(vec![(b("foo"), Value::Bytes(b("bar")))]);
        assert_eq!(d.dict_get(b"foo"), Some(&Value::Bytes(b("bar"))));
        assert_eq!(d.dict_get(b"missing"), None);
        assert_eq!(Value::Int(1).dict_get(b"foo"), None);
    }

    #[test]
    fn decode_negative_int() {
        assert_eq!(decode(b"i-0e").unwrap(), Value::Int(0));
        assert_eq!(decode(b"i-5e").unwrap(), Value::Int(-5));
    }

    #[test]
    fn decode_bytes_zero_length() {
        assert_eq!(decode(b"0:").unwrap(), Value::Bytes(b("")));
    }

    #[test]
    fn decode_nested_list() {
        assert_eq!(
            decode(b"lli1eei2ee").unwrap(),
            Value::List(vec![
                Value::List(vec![Value::Int(1)]),
                Value::Int(2),
            ])
        );
    }

    #[test]
    fn decode_trailing_garbage_errors() {
        assert!(decode(b"i42ex").is_err());
    }
}
