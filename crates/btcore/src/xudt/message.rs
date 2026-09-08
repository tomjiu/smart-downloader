// BT 消息 ID
pub const MSG_CHOKE: u8 = 0x00;
pub const MSG_UNCHOKE: u8 = 0x01;
pub const MSG_INTERESTED: u8 = 0x02;
pub const MSG_NOT_INTERESTED: u8 = 0x03;
pub const MSG_HAVE: u8 = 0x04;
pub const MSG_BITFIELD: u8 = 0x05;
pub const MSG_REQUEST: u8 = 0x06;
pub const MSG_PIECE: u8 = 0x07;
pub const MSG_CANCEL: u8 = 0x08;
pub const BLOCK_SIZE: u32 = 16384;

pub fn build_request(piece: u32, begin: u32, length: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(13);
    buf.push(MSG_REQUEST);
    buf.extend_from_slice(&piece.to_be_bytes());
    buf.extend_from_slice(&begin.to_be_bytes());
    buf.extend_from_slice(&length.to_be_bytes());
    buf
}

pub fn parse_request(payload: &[u8]) -> Option<(u32, u32, u32)> {
    if payload.len() != 13 || payload[0] != MSG_REQUEST {
        return None;
    }
    let piece = u32::from_be_bytes(payload[1..5].try_into().unwrap());
    let begin = u32::from_be_bytes(payload[5..9].try_into().unwrap());
    let length = u32::from_be_bytes(payload[9..13].try_into().unwrap());
    Some((piece, begin, length))
}

pub fn build_bitfield(bits: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + bits.len());
    buf.push(MSG_BITFIELD);
    buf.extend_from_slice(bits);
    buf
}

pub fn build_interested() -> Vec<u8> {
    vec![MSG_INTERESTED]
}

pub fn build_unchoke() -> Vec<u8> {
    vec![MSG_UNCHOKE]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = build_request(15, 0x180000, BLOCK_SIZE);
        assert_eq!(r.len(), 13);
        assert_eq!(r[0], MSG_REQUEST);
        let (piece, begin, length) = parse_request(&r).unwrap();
        assert_eq!((piece, begin, length), (15, 0x180000, BLOCK_SIZE));
    }

    #[test]
    fn parse_request_rejects_wrong_id() {
        let mut r = build_request(1, 0, BLOCK_SIZE);
        r[0] = MSG_BITFIELD;
        assert!(parse_request(&r).is_none());
    }
}
