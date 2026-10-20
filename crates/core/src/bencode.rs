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

    /// Type-checked accessors（TD-1 统一：消散各 crate 私有的 as_int/as_bytes）。
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&[(Vec<u8>, Value)]> {
        match self {
            Value::Dict(d) => Some(d),
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

    #[error("nesting depth exceeds limit {0} at position {1}")]
    TooDeep(usize, usize),
}

/// 安全修复（V4）：bencode 解析递归深度上限。
/// 无上限递归会被恶意种子（`l`×10万）直接打成栈溢出 abort 整个进程
/// （Rust 栈溢出不可 catch_unwind）。正常 bencode 嵌套极少超过 10 层。
const MAX_DEPTH: usize = 64;

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

// ============ span 定位器（TD-1 统一：infohash 原始字节哈希专用） ============
//
// 背景：infohash = SHA1(info dict 的**原始字节**)——重编码不保证逐字节一致，
// 因此 infohash 工作流必须定位原始 span 而非走 `decode` 物化。此前全仓有
// 4 份结构同构但实现各异的私扫描器（core/torrent_meta、daemon/bt_alerts、
// xunlei-convert/fastresume、provider/xunlei/client——后者甚至是朴素字节
// 搜索，非结构感知），本节是唯一权威实现。
//
// 防护与 `decode` 同款：MAX_DEPTH 递归上限 + 长度字段 checked_add（H-3）。

/// 在**顶层 dict** 中定位 `key` 对应值的原始字节区间。
///
/// 返回 `(start, end)`：`start` = 值首字节下标；`end` = 值**末字节**下标
/// （闭区间，`&data[start..=end]` 即该值的完整 bencode 编码）。对 dict/list
/// `end` 是闭合 `e` 的下标；对 int 是其 `e`；对 str 是内容末字节。
///
/// 典型用法：`locate_dict_value(torrent, b"info")` → SHA1 该 span 得 v1
/// infohash。键匹配只在**顶层**进行（嵌套 dict 中的同名键不会被误匹配——
/// 跳过值时是结构感知的，字符串内容中的 `4:info` 字样不会形成假阳性）。
///
/// 输入非法（非 dict 顶层 / 截断 / 超深嵌套 / 超长字段）一律返回 `None`。
pub fn locate_dict_value(data: &[u8], key: &[u8]) -> Option<(usize, usize)> {
    if data.first() != Some(&b'd') {
        return None;
    }
    let mut pos = 1usize;
    while pos < data.len() && data[pos] != b'e' {
        // dict 键必须是字符串
        if !data[pos].is_ascii_digit() {
            return None;
        }
        let (content_start, content_end) = str_content_span(data, pos)?;
        let after_key = content_end;
        if &data[content_start..content_end] == key {
            let after_value = skip_value_span(data, after_key, 0)?;
            // after_value = 值后第一个下标；末字节 = after_value - 1
            let end = after_value.checked_sub(1)?;
            return Some((after_key, end));
        }
        pos = skip_value_span(data, after_key, 0)?;
    }
    None
}

/// `pos` 处的 bencode 字符串 `len:bytes` → (内容起始, 内容结束)（左闭右开）。
/// H-3 防护：`colon + 1 + len` 全程 checked_add，回绕/越界 → None。
fn str_content_span(data: &[u8], pos: usize) -> Option<(usize, usize)> {
    let colon = data[pos..]
        .iter()
        .position(|&b| b == b':')
        .map(|p| p + pos)?;
    let len: usize = std::str::from_utf8(&data[pos..colon]).ok()?.parse().ok()?;
    let start = colon.checked_add(1)?;
    let end = start.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    Some((start, end))
}

/// 跳过 `pos` 处的一个完整 bencode 值，返回值结束后的下标（开区间）。
/// 递归深度上限 MAX_DEPTH（V4 同款：恶意超深嵌套 → None 而非栈溢出）。
fn skip_value_span(data: &[u8], pos: usize, depth: usize) -> Option<usize> {
    if depth > MAX_DEPTH {
        return None;
    }
    match data.get(pos)? {
        b'd' => {
            let mut p = pos + 1;
            while p < data.len() && data[p] != b'e' {
                if !data[p].is_ascii_digit() {
                    return None; // dict 键必须是字符串
                }
                let (_, key_end) = str_content_span(data, p)?;
                p = skip_value_span(data, key_end, depth + 1)?;
            }
            Some(p + 1)
        }
        b'l' => {
            let mut p = pos + 1;
            while p < data.len() && data[p] != b'e' {
                p = skip_value_span(data, p, depth + 1)?;
            }
            Some(p + 1)
        }
        b'i' => {
            let e = data[pos..].iter().position(|&c| c == b'e')? + pos;
            Some(e + 1)
        }
        c if c.is_ascii_digit() => str_content_span(data, pos).map(|(_, end)| end),
        _ => None,
    }
}

fn decode_at(data: &[u8], pos: usize) -> Result<(Value, usize), DecodeError> {
    decode_at_d(data, pos, 0)
}

fn decode_at_d(data: &[u8], pos: usize, depth: usize) -> Result<(Value, usize), DecodeError> {
    if depth > MAX_DEPTH {
        return Err(DecodeError::TooDeep(MAX_DEPTH, pos));
    }
    let c = byte_at(data, pos)?;
    match c {
        b'i' => decode_int(data, pos),
        b'0'..=b'9' => decode_bytes(data, pos),
        b'l' => decode_list_d(data, pos, depth),
        b'd' => decode_dict_d(data, pos, depth),
        _ => Err(DecodeError::InvalidByte(c, pos)),
    }
}

fn byte_at(data: &[u8], pos: usize) -> Result<u8, DecodeError> {
    data.get(pos)
        .copied()
        .ok_or(DecodeError::UnexpectedEof(pos))
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
    // 安全修复（H-3）：start + len 必须用 checked_add——裸加法在 release
    // （overflow-checks=off）下回绕可绕过界检查，切片 start..start+len 因
    // end < start 直接 panic（恶意 bencode 输入可远程触发 DoS）。
    let end = match start.checked_add(len) {
        Some(e) if e <= data.len() => e,
        _ => return Err(DecodeError::UnexpectedEof(colon)),
    };
    Ok((Value::Bytes(data[start..end].to_vec()), end))
}

fn decode_list_d(data: &[u8], pos: usize, depth: usize) -> Result<(Value, usize), DecodeError> {
    let mut items = Vec::new();
    let mut pos = pos + 1;
    while pos < data.len() && data[pos] != b'e' {
        let (v, next) = decode_at_d(data, pos, depth + 1)?;
        items.push(v);
        pos = next;
    }
    if pos >= data.len() {
        return Err(DecodeError::UnexpectedEof(pos));
    }
    Ok((Value::List(items), pos + 1))
}

fn decode_dict_d(data: &[u8], pos: usize, depth: usize) -> Result<(Value, usize), DecodeError> {
    let mut items = Vec::new();
    let mut pos = pos + 1;
    while pos < data.len() && data[pos] != b'e' {
        let (k, next) = decode_at_d(data, pos, depth + 1)?;
        let k = match k {
            Value::Bytes(b) => b,
            _ => return Err(DecodeError::InvalidByte(b'd', pos)),
        };
        pos = next;
        if pos >= data.len() {
            return Err(DecodeError::UnexpectedEof(pos));
        }
        let (v, next) = decode_at_d(data, pos, depth + 1)?;
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
        let v = Value::Dict(vec![(b("b"), Value::Int(2)), (b("a"), Value::Int(1))]);
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
            (b("m"), Value::Dict(vec![(b("ut_pex"), Value::Int(1))])),
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
            Value::List(vec![Value::List(vec![Value::Int(1)]), Value::Int(2),])
        );
    }

    #[test]
    fn decode_trailing_garbage_errors() {
        assert!(decode(b"i42ex").is_err());
    }

    // 安全回归（V4）：超深嵌套必须报 TooDeep 而非栈溢出。
    #[test]
    fn decode_excessive_nesting_rejected() {
        // 10 万层嵌套 list：修复前会栈溢出 abort 进程，现在应优雅报错。
        let mut data = vec![b'l'; 100_000];
        data.extend(std::iter::repeat_n(b'e', 100_000));
        match decode(&data) {
            Err(DecodeError::TooDeep(_, _)) => {}
            other => panic!("expected TooDeep, got {:?}", other.map(|_| "decoded")),
        }
    }

    #[test]
    fn decode_deep_but_legal_nesting_ok() {
        // 上限 64 层以内正常解码（构造 60 层 dict 嵌套）。
        let mut data = Vec::new();
        for _ in 0..60 {
            data.extend_from_slice(b"d1:k");
        }
        data.extend_from_slice(b"i1e");
        data.extend(std::iter::repeat_n(b'e', 60));
        assert!(decode(&data).is_ok());
    }

    #[test]
    fn decode_excessive_dict_nesting_rejected() {
        let mut data = vec![b'd'; 200];
        data.extend(std::iter::repeat_n(b'e', 200));
        assert!(matches!(decode(&data), Err(DecodeError::TooDeep(_, _))));
    }

    // 安全回归（H-3）：超大长度字段在 release（overflow-checks=off）下曾因
    // start+len 回绕绕过界检查 → 切片 panic（slice index starts at X but ends at Y）。
    // 修复后必须优雅报错而非 panic。
    #[test]
    fn decode_huge_length_rejected() {
        // len = usize::MAX：start+len 必然回绕
        let mut data = b"18446744073709551615:".to_vec();
        data.extend_from_slice(b"payload");
        assert!(matches!(decode(&data), Err(DecodeError::UnexpectedEof(_))));
        // len = usize::MAX - 20：start(21)+len 恰好回绕到 0，旧检查 0 > 28 不成立
        // → 越界切片 panic；修复后走 checked_add → UnexpectedEof。
        let mut data2 = b"18446744073709551595:".to_vec();
        data2.extend_from_slice(b"payload");
        assert!(matches!(decode(&data2), Err(DecodeError::UnexpectedEof(_))));
    }

    // ============ locate_dict_value（TD-1 统一 span 定位器） ============

    /// 顶层 info dict 定位：span 必须是 info 值的完整原始编码。
    #[test]
    fn locate_info_dict_basic() {
        // d 8:announce 15:http://tracker/ 4:info d...e e
        let data = b"d8:announce15:http://tracker/4:infod4:name8:testfile6:lengthi1024eee";
        let (start, end) = locate_dict_value(data, b"info").expect("locate info");
        assert_eq!(&data[start..=end], b"d4:name8:testfile6:lengthi1024ee");
        // 起点是 'd'，终点是配对 'e'
        assert_eq!(data[start], b'd');
        assert_eq!(data[end], b'e');
    }

    /// 键不存在 → None。
    #[test]
    fn locate_missing_key() {
        let data = b"d3:foo3:bare";
        assert!(locate_dict_value(data, b"info").is_none());
        assert!(locate_dict_value(data, b"fo").is_none()); // 前缀不匹配
    }

    /// 非顶层 dict（list/str/int）→ None。
    #[test]
    fn locate_requires_top_level_dict() {
        assert!(locate_dict_value(b"li1ee", b"info").is_none());
        assert!(locate_dict_value(b"4:spam", b"info").is_none());
        assert!(locate_dict_value(b"i42e", b"info").is_none());
        assert!(locate_dict_value(b"", b"info").is_none());
    }

    /// **核心回归（provider/client.rs 旧实现的缺陷）**：
    /// 字符串内容里出现 `4:info` + `d` 字样（如 announce URL 含 "4:infod"）
    /// 不得形成假阳性——旧实现是朴素字节搜索，会错误定位并产出错误 infohash。
    /// 结构感知扫描跳过字符串内容，只匹配顶层真键。
    #[test]
    fn locate_no_false_positive_inside_string_content() {
        // announce 的字符串内容含 "4:infod"；顶层真 info 在其后。
        // 朴素扫描会在 announce 内容里命中 "4:info"+'d' → 错误 span。
        let announce = b"http://evil.example.com/xx4:infodyyyy";
        let mut data = Vec::new();
        data.extend_from_slice(b"d8:announce");
        data.extend_from_slice(announce.len().to_string().as_bytes());
        data.push(b':');
        data.extend_from_slice(announce);
        data.extend_from_slice(b"4:infod4:name4:file6:lengthi1ee");
        data.push(b'e');
        let (start, end) = locate_dict_value(&data, b"info").expect("locate real info");
        assert_eq!(&data[start..=end], b"d4:name4:file6:lengthi1ee");
    }

    /// 嵌套 dict 中的 info 键（非顶层）不被匹配。
    #[test]
    fn locate_only_top_level() {
        // 顶层 foo 的值是 dict，内含 info 键；顶层无 info。
        let data = b"d3:food4:infoi1eee";
        assert!(locate_dict_value(data, b"info").is_none());
    }

    /// 值为标量（int/str）时返回其完整编码区间。
    #[test]
    fn locate_scalar_values() {
        let data = b"d3:bari42e3:foo3:baze";
        let (s, e) = locate_dict_value(data, b"bar").unwrap();
        assert_eq!(&data[s..=e], b"i42e");
        let (s, e) = locate_dict_value(data, b"foo").unwrap();
        // str 值的 span 是其完整编码（含长度前缀）
        assert_eq!(&data[s..=e], b"3:baz");
    }

    /// 截断/畸形输入 → None（不 panic）。
    #[test]
    fn locate_truncated_and_malformed() {
        assert!(locate_dict_value(b"d4:info", b"info").is_none()); // 值截断
        assert!(locate_dict_value(b"d4:infod4:name", b"info").is_none()); // 嵌套截断
        assert!(locate_dict_value(b"d", b"info").is_none()); // 空 dict
        assert!(locate_dict_value(b"di1ei2ee", b"info").is_none()); // 非字符串键
                                                                    // 超长字符串字段（H-3）：checked_add 回绕/越界 → None
        let mut evil = b"d4:info18446744073709551615:".to_vec();
        evil.extend_from_slice(b"xxxx");
        assert!(locate_dict_value(&evil, b"info").is_none());
    }

    /// 超深嵌套（V4 同款）：MAX_DEPTH 上限 → None 而非栈溢出。
    #[test]
    fn locate_excessive_nesting_rejected() {
        let mut data = b"d4:info".to_vec();
        data.extend(std::iter::repeat_n(b'l', 100_000));
        data.extend(std::iter::repeat_n(b'e', 100_000));
        data.push(b'e');
        assert!(locate_dict_value(&data, b"info").is_none());
    }

    /// 定位 span 后用 decode 解析该 span，与整档 decode 的子树一致
    /// （infohash 工作流的标准组合：locate → SHA1 / locate → decode 走树）。
    #[test]
    fn locate_then_decode_matches_subtree() {
        let data = b"d8:announce8:tracker/4:infod4:name8:testfile12:piece lengthi16384eee";
        let (start, end) = locate_dict_value(data, b"info").unwrap();
        let sub = decode(&data[start..=end]).unwrap();
        let full = decode(data).unwrap();
        assert_eq!(sub, *full.dict_get(b"info").unwrap());
    }

    /// Value 类型检查访问器。
    #[test]
    fn value_accessors() {
        let v = decode(b"d1:ali1ee1:bi2e1:c3:xyz1:dli9eee").unwrap();
        assert_eq!(v.as_int(), None);
        assert_eq!(v.as_bytes(), None);
        assert_eq!(v.as_list(), None);
        assert_eq!(v.as_dict().map(|d| d.len()), Some(4));
        assert_eq!(
            v.dict_get(b"a").unwrap().as_list().unwrap()[0].as_int(),
            Some(1)
        );
        assert_eq!(v.dict_get(b"b").unwrap().as_int(), Some(2));
        assert_eq!(
            v.dict_get(b"c").unwrap().as_bytes(),
            Some(b"xyz".as_slice())
        );
        assert_eq!(v.dict_get(b"d").unwrap().as_list().unwrap().len(), 1);
    }
}
