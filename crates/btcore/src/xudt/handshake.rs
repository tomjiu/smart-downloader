pub const CLIENT_VERSION: &[u8] = b"XunLei 0019";
pub const LOCAL_PORT: u16 = 15000;

pub fn build_handshake(port: u16, reqq: u32) -> Vec<u8> {
    // 常量前缀：msg id 0x14 + 0x00 + bencode 字典开段 + 能力表（ut_holepunch/ut_metadata/ut_pex），
    // 动态项仅 1:p / 4:reqq / 1:v 三个字段；字节布局由 handshake_matches_known_bytes 金样锁定。
    const PREFIX: &[u8] = b"\x14\x00d1:ei0e1:md12:ut_holepunchi4e11:ut_metadatai2e6:ut_pexi1ee";
    let mut buf = PREFIX.to_vec();

    // 1:p i<port> e
    buf.extend_from_slice(b"1:pi");
    buf.extend_from_slice(port.to_string().as_bytes());
    buf.push(b'e');

    // 4:reqq i<reqq> e
    buf.extend_from_slice(b"4:reqqi");
    buf.extend_from_slice(reqq.to_string().as_bytes());
    buf.push(b'e');

    // 1:v <len>:<version>
    buf.extend_from_slice(b"1:v");
    buf.extend_from_slice(CLIENT_VERSION.len().to_string().as_bytes());
    buf.push(b':');
    buf.extend_from_slice(CLIENT_VERSION);

    // close top dict
    buf.push(b'e');

    buf
}

fn find_substr(data: &[u8], pat: &[u8]) -> Option<usize> {
    data.windows(pat.len()).position(|w| w == pat)
}

pub fn parse_handshake(payload: &[u8]) -> Option<(u16, u32, Vec<u8>)> {
    if payload.len() < 2 || payload[0] != 0x14 || payload[1] != 0x00 {
        return None;
    }

    let data = &payload[2..];

    // find "1:p" marker
    let p_pos = find_substr(data, b"1:p")?;
    let p_section = &data[p_pos..];
    let p_rest = &p_section[3..];
    if !p_rest.starts_with(b"i") {
        return None;
    }
    let e_pos = p_rest.iter().position(|&b| b == b'e')?;
    let port = std::str::from_utf8(&p_rest[1..e_pos])
        .ok()?
        .parse::<u16>()
        .ok()?;

    // find "4:reqq" marker
    let reqq_pos = find_substr(data, b"4:reqq")?;
    let reqq_section = &data[reqq_pos..];
    let reqq_rest = &reqq_section[6..];
    if !reqq_rest.starts_with(b"i") {
        return None;
    }
    let reqq_e = reqq_rest.iter().position(|&b| b == b'e')?;
    let reqq = std::str::from_utf8(&reqq_rest[1..reqq_e])
        .ok()?
        .parse::<u32>()
        .ok()?;

    // find "1:v" marker
    let v_pos = find_substr(data, b"1:v")?;
    let v_section = &data[v_pos..];
    let v_rest = &v_section[3..];
    // parse bencode string: <len>:<value>
    let colon = v_rest.iter().position(|&b| b == b':')?;
    let len = std::str::from_utf8(&v_rest[..colon])
        .ok()?
        .parse::<usize>()
        .ok()?;
    // 安全修复（H-3 同型）：colon+1+len 裸加法在 release 下可回绕绕过检查 →
    // 切片 panic（恶意 xudt 握手 peer）。
    let end = colon.checked_add(1)?.checked_add(len)?;
    if v_rest.len() < end {
        return None;
    }
    let version = v_rest[colon + 1..end].to_vec();

    Some((port, reqq, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn handshake_roundtrip() {
        let h = build_handshake(LOCAL_PORT, 128);
        assert_eq!(h[0], 0x14);
        assert_eq!(h[1], 0x00);
        let (port, reqq, version) = parse_handshake(&h).unwrap();
        assert_eq!(port, LOCAL_PORT);
        assert_eq!(reqq, 128);
        assert_eq!(version, CLIENT_VERSION);
    }

    #[test]
    fn handshake_matches_known_bytes() {
        let h = build_handshake(15000, 128);
        let known = hex_decode("140064313a65693065313a6d6431323a75745f686f6c6570756e636869346531313a75745f6d65746164617461693265363a75745f70657869316565313a7069313530303065343a726571716931323865313a7631313a58756e4c6569203030313965");
        assert_eq!(h, known);
    }
}
