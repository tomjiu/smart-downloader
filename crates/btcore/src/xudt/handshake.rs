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

pub const CLIENT_VERSION: &[u8] = b"XunLei 0019";
pub const LOCAL_PORT: u16 = 15000;

pub fn build_handshake(port: u16, reqq: u32) -> Vec<u8> {
    // 0x14 协议扩展位 + 0x00 保留位 + bencode 字典 'd'
    let mut buf = vec![0x14, 0x00, b'd'];

    // 1:e i0 e
    buf.push(b'1');
    buf.push(b':');
    buf.push(b'e');
    buf.push(b'i');
    buf.push(b'0');
    buf.push(b'e');

    // 1:m d ... e e
    buf.push(b'1');
    buf.push(b':');
    buf.push(b'm');
    buf.push(b'd');
    // 12:ut_holepunch i4 e
    buf.push(b'1');
    buf.push(b'2');
    buf.push(b':');
    buf.extend_from_slice(b"ut_holepunch");
    buf.push(b'i');
    buf.extend_from_slice(b"4");
    buf.push(b'e');
    // 11:ut_metadata i2 e
    buf.push(b'1');
    buf.push(b'1');
    buf.push(b':');
    buf.extend_from_slice(b"ut_metadata");
    buf.push(b'i');
    buf.extend_from_slice(b"2");
    buf.push(b'e');
    // 6:ut_pex i1 e
    buf.push(b'6');
    buf.push(b':');
    buf.extend_from_slice(b"ut_pex");
    buf.push(b'i');
    buf.extend_from_slice(b"1");
    buf.push(b'e');
    // close m dict
    buf.push(b'e');

    // 1:p i<port> e
    buf.push(b'1');
    buf.push(b':');
    buf.push(b'p');
    buf.push(b'i');
    buf.extend_from_slice(port.to_string().as_bytes());
    buf.push(b'e');

    // 4:reqq i<reqq> e
    buf.push(b'4');
    buf.push(b':');
    buf.extend_from_slice(b"reqq");
    buf.push(b'i');
    buf.extend_from_slice(reqq.to_string().as_bytes());
    buf.push(b'e');

    // 1:v <len>:<version>
    buf.push(b'1');
    buf.push(b':');
    buf.push(b'v');
    let version_bytes = CLIENT_VERSION;
    buf.extend_from_slice(version_bytes.len().to_string().as_bytes());
    buf.push(b':');
    buf.extend_from_slice(version_bytes);

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
    if v_rest.len() < colon + 1 + len {
        return None;
    }
    let version = v_rest[colon + 1..colon + 1 + len].to_vec();

    Some((port, reqq, version))
}
