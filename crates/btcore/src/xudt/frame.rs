// ---- 常量 (抓包实证) ----
pub const TYPE_LOCAL: u32 = 0x0100_139E;
pub const TYPE_PEER: u32 = 0x0100_139D;
pub const FIXED_CTRL: u32 = 0x0008_0000;
pub const FIXED_DATA: u32 = 0x0000_C350;

pub struct Frame {
    pub ftype: u32,
    pub seq: u32,
    pub aux: u32,
    pub fixed: u32,
    pub ctr: u32,
    pub payload: Vec<u8>,
}

pub fn wrap_frame(payload: &[u8], ftype: u32, seq: u32, aux: u32, ctr: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(24 + payload.len());
    buf.extend_from_slice(&ftype.to_be_bytes());
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(&aux.to_be_bytes());
    buf.extend_from_slice(&FIXED_CTRL.to_be_bytes());
    buf.extend_from_slice(&ctr.to_be_bytes());
    let plen = (payload.len() as u32).to_be_bytes();
    buf.extend_from_slice(&plen);
    buf.extend_from_slice(payload);
    buf
}

pub fn unwrap_frame(data: &[u8]) -> Option<Frame> {
    if data.len() < 24 {
        return None;
    }
    let ftype = u32::from_be_bytes(data[0..4].try_into().unwrap());
    let seq = u32::from_be_bytes(data[4..8].try_into().unwrap());
    let aux = u32::from_be_bytes(data[8..12].try_into().unwrap());
    let fixed = u32::from_be_bytes(data[12..16].try_into().unwrap());
    let ctr = u32::from_be_bytes(data[16..20].try_into().unwrap());
    let plen = u32::from_be_bytes(data[20..24].try_into().unwrap()) as usize;
    if data.len() < 24 + plen {
        return None;
    }
    let payload = data[24..24 + plen].to_vec();
    Some(Frame {
        ftype,
        seq,
        aux,
        fixed,
        ctr,
        payload,
    })
}

pub fn wrap_data(block: &[u8], seq: u32, ctr: u32, sess: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(20 + block.len());
    buf.extend_from_slice(&TYPE_PEER.to_be_bytes());
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(&sess.to_be_bytes());
    buf.extend_from_slice(&FIXED_DATA.to_be_bytes());
    buf.extend_from_slice(&ctr.to_be_bytes());
    buf.extend_from_slice(block);
    buf
}

pub fn unwrap_data(data: &[u8]) -> Option<(u32, u32, u32, u32, u32, Vec<u8>)> {
    if data.len() < 20 {
        return None;
    }
    let ftype = u32::from_be_bytes(data[0..4].try_into().unwrap());
    let seq = u32::from_be_bytes(data[4..8].try_into().unwrap());
    let sess = u32::from_be_bytes(data[8..12].try_into().unwrap());
    let fixed = u32::from_be_bytes(data[12..16].try_into().unwrap());
    let ctr = u32::from_be_bytes(data[16..20].try_into().unwrap());
    let block = data[20..].to_vec();
    Some((ftype, seq, sess, fixed, ctr, block))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_and_unwrap_frame_roundtrip() {
        let payload = vec![1u8, 2, 3];
        let f = wrap_frame(&payload, TYPE_LOCAL, 0xE4A3CA76, 0xE38B045B, 0x422F7BCE);
        assert_eq!(f.len(), 24 + 3);
        let u = unwrap_frame(&f).unwrap();
        assert_eq!(u.ftype, TYPE_LOCAL);
        assert_eq!(u.seq, 0xE4A3CA76);
        assert_eq!(u.aux, 0xE38B045B);
        assert_eq!(u.fixed, FIXED_CTRL);
        assert_eq!(u.ctr, 0x422F7BCE);
        assert_eq!(u.payload, payload);
    }

    #[test]
    fn unwrap_frame_too_short_returns_none() {
        assert!(unwrap_frame(&[0u8; 10]).is_none());
    }

    #[test]
    fn wrap_and_unwrap_data_roundtrip() {
        let block = vec![0xABu8; 1381];
        let f = wrap_data(&block, 100, 200, 0x1C7695B5);
        assert_eq!(f.len(), 20 + 1381);
        let (ftype, seq, sess, fixed, ctr, b) = unwrap_data(&f).unwrap();
        assert_eq!(ftype, TYPE_PEER);
        assert_eq!(seq, 100);
        assert_eq!(sess, 0x1C7695B5);
        assert_eq!(fixed, FIXED_DATA);
        assert_eq!(ctr, 200);
        assert_eq!(b, block);
    }

    #[test]
    fn unwrap_data_too_short_returns_none() {
        assert!(unwrap_data(&[0u8; 5]).is_none());
    }
}
