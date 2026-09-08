//! 迅雷 CDN 直链分类器（移植自 toolkit/xunlei_url_classifier.py）。
//! 纯函数，无 I/O。
//! 溯源：
//!   - classify_url 逻辑：toolkit/xunlei_url_classifier.py#classify_url#L55-L127
//!   - XUNLEI_DOMAINS：#XUNLEI_DOMAINS#L26-L34
//!   - PHUB_SHUB_HOSTS：#PHUB_SHUB_HOSTS#L37-L52
//!   - n0808 / sandai 通配：#classify_url#L100-L118
//!   - CDN host 表（完整 202 条）：toolkit/xunlei_cdn_hosts.json 的 cdn_hosts 与 cdn_by_region 字段
//!
//! 说明：Python 版 classify_url 返回 A / B / PHUB 三类；本 Rust 版按任务要求仅区分 A(迅雷自有 CDN) /
//! B(普通源) 两类，将 Python 的 A 与 PHUB 统一归入 A（二者均属迅雷自有基础设施，非普通源）。

/// 直链类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClass {
    /// A：迅雷自有 CDN（大概率账号绑定 / 高速）。
    AThunderCdn,
    /// B：普通源（可直接 HTTP 下载）。
    BRegular,
}

// 迅雷已知域名（非仅 CDN）。来源：toolkit/xunlei_url_classifier.py#XUNLEI_DOMAINS#L26-L34
const XUNLEI_DOMAINS: &[&str] = &[
    "n0808.com",
    "cdn.xunlei.com",
    "vod.xunlei.com",
    "api-pan.xunlei.com",
    "pan.xunlei.com",
    "captcha.xunlei.com",
    "xluser-ssl.xunlei.com",
];

// PHub / SHub 服务器（迅雷自有基础设施，归入 A）。来源：#PHUB_SHUB_HOSTS#L37-L52
const PHUB_SHUB_HOSTS: &[&str] = &[
    "pr-phub.sandai.net",
    "sr-shub.sandai.net",
    "hub5p.sandai.net",
    "hub5btmain.sandai.net",
    "hub5idx.shub.sandai.net",
    "dcdn.sandai.net",
    "dphub.sandai.net",
    "gw-phub.sandai.net",
    "hub5u.sandai.net",
    "hubciddata.sandai.net",
    "rcv.sandai.net",
    "btmain-shub.sandai.net",
    "emu-shub.sandai.net",
    "viphub5pr.phub.sandai.net",
];

// 完整 CDN host 列表（202 条）。来源：xunlei_cdn_hosts.json#cdn_hosts
const CDN_HOSTS: &[&str] = &[
    "vod0001-c01-vip-lixian.xunlei.com",
    "vod0001-m01-vip-lixian.xunlei.com",
    "vod0002-c01-vip-lixian.xunlei.com",
    "vod0002-m01-vip-lixian.xunlei.com",
    "vod0003-c01-vip-lixian.xunlei.com",
    "vod0003-h05-vip-lixian.xunlei.com",
    "vod0003-m01-vip-lixian.xunlei.com",
    "vod0004-c01-vip-lixian.xunlei.com",
    "vod0004-h05-vip-lixian.xunlei.com",
    "vod0005-c01-vip-lixian.xunlei.com",
    "vod0006-b05-vip-lixian.xunlei.com",
    "vod0006-m01-vip-lixian.xunlei.com",
    "vod0007-h05-vip-lixian.xunlei.com",
    "vod0007-m01-vip-lixian.xunlei.com",
    "vod0008-h05-vip-lixian.xunlei.com",
    "vod0008-m01-vip-lixian.xunlei.com",
    "vod0009-b05-vip-lixian.xunlei.com",
    "vod0009-h05-vip-lixian.xunlei.com",
    "vod0010-b05-vip-lixian.xunlei.com",
    "vod0010-h05-vip-lixian.xunlei.com",
    "vod0010-m01-vip-lixian.xunlei.com",
    "vod0011-b05-vip-lixian.xunlei.com",
    "vod0011-m01-vip-lixian.xunlei.com",
    "vod0012-b05-vip-lixian.xunlei.com",
    "vod0012-h05-vip-lixian.xunlei.com",
    "vod0012-m01-vip-lixian.xunlei.com",
    "vod0013-b05-vip-lixian.xunlei.com",
    "vod0013-h05-vip-lixian.xunlei.com",
    "vod0013-m01-vip-lixian.xunlei.com",
    "vod0014-b05-vip-lixian.xunlei.com",
    "vod0014-h05-vip-lixian.xunlei.com",
    "vod0014-m01-vip-lixian.xunlei.com",
    "vod0017-h05-vip-lixian.xunlei.com",
    "vod0019-m01-vip-lixian.xunlei.com",
    "vod0020-m01-vip-lixian.xunlei.com",
    "vod0021-m01-vip-lixian.xunlei.com",
    "vod0022-m01-vip-lixian.xunlei.com",
    "vod0032-z01-vip-lixian.xunlei.com",
    "vod0035-z01-vip-lixian.xunlei.com",
    "vod0036-z01-vip-lixian.xunlei.com",
    "vod0037-z01-vip-lixian.xunlei.com",
    "vod0038-z01-vip-lixian.xunlei.com",
    "vod0039-z01-vip-lixian.xunlei.com",
    "vod0040-z01-vip-lixian.xunlei.com",
    "vod0041-z01-vip-lixian.xunlei.com",
    "vod0042-z01-vip-lixian.xunlei.com",
    "vod0043-b05-vip-lixian.xunlei.com",
    "vod0044-b05-vip-lixian.xunlei.com",
    "vod0045-b05-vip-lixian.xunlei.com",
    "vod0051-b05-vip-lixian.xunlei.com",
    "vod0053-b05-vip-lixian.xunlei.com",
    "vod0054-b05-vip-lixian.xunlei.com",
    "vod0055-b05-vip-lixian.xunlei.com",
    "vod0064-txyun08-vip-lixian.xunlei.com",
    "vod0065-txyun08-vip-lixian.xunlei.com",
    "vod0066-txyun08-vip-lixian.xunlei.com",
    "vod0067-txyun08-vip-lixian.xunlei.com",
    "vod0068-txyun08-vip-lixian.xunlei.com",
    "vod0069-txyun08-vip-lixian.xunlei.com",
    "vod0070-h01-vip-lixian.xunlei.com",
    "vod0070-txyun08-vip-lixian.xunlei.com",
    "vod0071-h01-vip-lixian.xunlei.com",
    "vod0074-h01-vip-lixian.xunlei.com",
    "vod0075-h01-vip-lixian.xunlei.com",
    "vod0080-b02-vip-lixian.xunlei.com",
    "vod0088-h04-vip-lixian.xunlei.com",
    "vod0089-h04-vip-lixian.xunlei.com",
    "vod0090-h04-vip-lixian.xunlei.com",
    "vod0091-h04-vip-lixian.xunlei.com",
    "vod0091-z01-vip-lixian.xunlei.com",
    "vod0092-h04-vip-lixian.xunlei.com",
    "vod0093-h04-vip-lixian.xunlei.com",
    "vod0093-z01-vip-lixian.xunlei.com",
    "vod0094-h04-vip-lixian.xunlei.com",
    "vod0097-h04-vip-lixian.xunlei.com",
    "vod0097-h05-vip-lixian.xunlei.com",
    "vod0098-h04-vip-lixian.xunlei.com",
    "vod0098-h05-vip-lixian.xunlei.com",
    "vod0099-h04-vip-lixian.xunlei.com",
    "vod0099-h05-vip-lixian.xunlei.com",
    "vod0100-h04-vip-lixian.xunlei.com",
    "vod0101-h04-vip-lixian.xunlei.com",
    "vod0105-h04-vip-lixian.xunlei.com",
    "vod0116-h05-vip-lixian.xunlei.com",
    "vod0117-h05-vip-lixian.xunlei.com",
    "vod0121-h05-vip-lixian.xunlei.com",
    "vod0122-h05-vip-lixian.xunlei.com",
    "vod0128-h04-vip-lixian.xunlei.com",
    "vod0129-h04-vip-lixian.xunlei.com",
    "vod0131-h01-vip-lixian.xunlei.com",
    "vod0131-h05-vip-lixian.xunlei.com",
    "vod0131-z01-vip-lixian.xunlei.com",
    "vod0132-h01-vip-lixian.xunlei.com",
    "vod0135-z01-vip-lixian.xunlei.com",
    "vod0136-z01-vip-lixian.xunlei.com",
    "vod0139-b05-vip-lixian.xunlei.com",
    "vod0140-b05-vip-lixian.xunlei.com",
    "vod0141-b05-vip-lixian.xunlei.com",
    "vod0142-b05-vip-lixian.xunlei.com",
    "vod0143-b05-vip-lixian.xunlei.com",
    "vod0143-h04-vip-lixian.xunlei.com",
    "vod0145-h05-vip-lixian.xunlei.com",
    "vod0146-h05-vip-lixian.xunlei.com",
    "vod0146-z01-vip-lixian.xunlei.com",
    "vod0153-h01-vip-lixian.xunlei.com",
    "vod0155-z01-vip-lixian.xunlei.com",
    "vod0156-z01-vip-lixian.xunlei.com",
    "vod0167-z01-vip-lixian.xunlei.com",
    "vod0184-h05-vip-lixian.xunlei.com",
    "vod0185-h05-vip-lixian.xunlei.com",
    "vod0195-z01-vip-lixian.xunlei.com",
    "vod0196-z01-vip-lixian.xunlei.com",
    "vod0221-h05-vip-lixian.xunlei.com",
    "vod0222-h05-vip-lixian.xunlei.com",
    "vod0223-h05-vip-lixian.xunlei.com",
    "vod0224-h05-vip-lixian.xunlei.com",
    "vod0225-h05-vip-lixian.xunlei.com",
    "vod0227-h05-vip-lixian.xunlei.com",
    "vod0252-h05-vip-lixian.xunlei.com",
    "vod0253-h05-vip-lixian.xunlei.com",
    "vod0254-aliyun08-vip-lixian.xunlei.com",
    "vod0254-h05-vip-lixian.xunlei.com",
    "vod0255-aliyun08-vip-lixian.xunlei.com",
    "vod0256-aliyun08-vip-lixian.xunlei.com",
    "vod0257-aliyun08-vip-lixian.xunlei.com",
    "vod0261-aliyun08-vip-lixian.xunlei.com",
    "vod0262-aliyun08-vip-lixian.xunlei.com",
    "vod0263-aliyun08-vip-lixian.xunlei.com",
    "vod0264-aliyun08-vip-lixian.xunlei.com",
    "vod0281-z01-vip-lixian.xunlei.com",
    "vod0317-h04-vip-lixian.xunlei.com",
    "vod0318-h04-vip-lixian.xunlei.com",
    "vod0319-h04-vip-lixian.xunlei.com",
    "vod0320-h04-vip-lixian.xunlei.com",
    "vod0340-txyun08-vip-lixian.xunlei.com",
    "vod0341-txyun08-vip-lixian.xunlei.com",
    "vod0349-b05-vip-lixian.xunlei.com",
    "vod0432-b02-vip-lixian.xunlei.com",
    "vod0531-b02-vip-lixian.xunlei.com",
    "vod0532-b02-vip-lixian.xunlei.com",
    "vod0533-b02-vip-lixian.xunlei.com",
    "vod0534-b02-vip-lixian.xunlei.com",
    "vod0537-b02-vip-lixian.xunlei.com",
    "vod0555-aliyun06-vip-lixian.xunlei.com",
    "vod0556-aliyun06-vip-lixian.xunlei.com",
    "vod0563-b02-vip-lixian.xunlei.com",
    "vod0565-b02-vip-lixian.xunlei.com",
    "vod0566-b02-vip-lixian.xunlei.com",
    "vod0568-b02-vip-lixian.xunlei.com",
    "vod0571-b02-vip-lixian.xunlei.com",
    "vod0572-b02-vip-lixian.xunlei.com",
    "vod0573-b02-vip-lixian.xunlei.com",
    "vod0595-b02-vip-lixian.xunlei.com",
    "vod0596-b02-vip-lixian.xunlei.com",
    "vod0597-b02-vip-lixian.xunlei.com",
    "vod0598-b02-vip-lixian.xunlei.com",
    "vod0636-b02-vip-lixian.xunlei.com",
    "vod0637-b02-vip-lixian.xunlei.com",
    "vod0638-b02-vip-lixian.xunlei.com",
    "vod0639-b02-vip-lixian.xunlei.com",
    "vod0640-b02-vip-lixian.xunlei.com",
    "vod0641-b02-vip-lixian.xunlei.com",
    "vod0642-b02-vip-lixian.xunlei.com",
    "vod0643-b02-vip-lixian.xunlei.com",
    "vod0644-b02-vip-lixian.xunlei.com",
    "vod0645-b02-vip-lixian.xunlei.com",
    "vod0646-b02-vip-lixian.xunlei.com",
    "vod0647-b02-vip-lixian.xunlei.com",
    "vod0648-b02-vip-lixian.xunlei.com",
    "vod0649-b02-vip-lixian.xunlei.com",
    "vod0650-b02-vip-lixian.xunlei.com",
    "vod0651-b02-vip-lixian.xunlei.com",
    "vod0652-b02-vip-lixian.xunlei.com",
    "vod0653-b02-vip-lixian.xunlei.com",
    "vod0654-b02-vip-lixian.xunlei.com",
    "vod0725-b02-vip-lixian.xunlei.com",
    "vod0726-b02-vip-lixian.xunlei.com",
    "vod0727-b02-vip-lixian.xunlei.com",
    "vod0759-aliyun08-vip-lixian.xunlei.com",
    "vod0760-aliyun08-vip-lixian.xunlei.com",
    "vod0780-aliyun04-vip-lixian.xunlei.com",
    "vod0781-aliyun04-vip-lixian.xunlei.com",
    "vod1284-aliyun06-vip-lixian.xunlei.com",
    "vod1285-aliyun06-vip-lixian.xunlei.com",
    "vod1363-aliyun06-vip-lixian.xunlei.com",
    "vod1372-aliyun06-vip-lixian.xunlei.com",
    "vod1629-aliyun06-vip-lixian.xunlei.com",
    "vod1630-aliyun06-vip-lixian.xunlei.com",
    "vod1703-aliyun06-vip-lixian.xunlei.com",
    "vod1704-aliyun06-vip-lixian.xunlei.com",
    "vod1844-aliyun06-vip-lixian.xunlei.com",
    "vod3379-aliyun04-vip-lixian.xunlei.com",
    "vod3429-aliyun04-vip-lixian.xunlei.com",
    "vod3459-aliyun04-vip-lixian.xunlei.com",
    "vod3533-aliyun04-vip-lixian.xunlei.com",
    "vod4252-aliyun04-vip-lixian.xunlei.com",
    "vod4253-aliyun04-vip-lixian.xunlei.com",
    "vod4320-aliyun04-vip-lixian.xunlei.com",
    "vod4321-aliyun04-vip-lixian.xunlei.com",
    "vod9410-aliyun08-vip-lixian.xunlei.com",
    "vod9411-aliyun08-vip-lixian.xunlei.com",
    "vod9412-aliyun08-vip-lixian.xunlei.com",
];

// 按区域分组的 CDN host。来源：xunlei_cdn_hosts.json#cdn_by_region
pub const CDN_BY_REGION: &[(&str, &[&str])] = &[
    (
        "c01",
        &[
            "vod0001-c01-vip-lixian.xunlei.com",
            "vod0002-c01-vip-lixian.xunlei.com",
            "vod0003-c01-vip-lixian.xunlei.com",
            "vod0004-c01-vip-lixian.xunlei.com",
            "vod0005-c01-vip-lixian.xunlei.com",
        ],
    ),
    (
        "m01",
        &[
            "vod0001-m01-vip-lixian.xunlei.com",
            "vod0002-m01-vip-lixian.xunlei.com",
            "vod0003-m01-vip-lixian.xunlei.com",
            "vod0006-m01-vip-lixian.xunlei.com",
            "vod0007-m01-vip-lixian.xunlei.com",
            "vod0008-m01-vip-lixian.xunlei.com",
            "vod0010-m01-vip-lixian.xunlei.com",
            "vod0011-m01-vip-lixian.xunlei.com",
            "vod0012-m01-vip-lixian.xunlei.com",
            "vod0013-m01-vip-lixian.xunlei.com",
            "vod0014-m01-vip-lixian.xunlei.com",
            "vod0019-m01-vip-lixian.xunlei.com",
            "vod0020-m01-vip-lixian.xunlei.com",
            "vod0021-m01-vip-lixian.xunlei.com",
            "vod0022-m01-vip-lixian.xunlei.com",
        ],
    ),
    (
        "h05",
        &[
            "vod0003-h05-vip-lixian.xunlei.com",
            "vod0004-h05-vip-lixian.xunlei.com",
            "vod0007-h05-vip-lixian.xunlei.com",
            "vod0008-h05-vip-lixian.xunlei.com",
            "vod0009-h05-vip-lixian.xunlei.com",
            "vod0010-h05-vip-lixian.xunlei.com",
            "vod0012-h05-vip-lixian.xunlei.com",
            "vod0013-h05-vip-lixian.xunlei.com",
            "vod0014-h05-vip-lixian.xunlei.com",
            "vod0017-h05-vip-lixian.xunlei.com",
            "vod0097-h05-vip-lixian.xunlei.com",
            "vod0098-h05-vip-lixian.xunlei.com",
            "vod0099-h05-vip-lixian.xunlei.com",
            "vod0116-h05-vip-lixian.xunlei.com",
            "vod0117-h05-vip-lixian.xunlei.com",
            "vod0121-h05-vip-lixian.xunlei.com",
            "vod0122-h05-vip-lixian.xunlei.com",
            "vod0131-h05-vip-lixian.xunlei.com",
            "vod0145-h05-vip-lixian.xunlei.com",
            "vod0146-h05-vip-lixian.xunlei.com",
            "vod0184-h05-vip-lixian.xunlei.com",
            "vod0185-h05-vip-lixian.xunlei.com",
            "vod0221-h05-vip-lixian.xunlei.com",
            "vod0222-h05-vip-lixian.xunlei.com",
            "vod0223-h05-vip-lixian.xunlei.com",
            "vod0224-h05-vip-lixian.xunlei.com",
            "vod0225-h05-vip-lixian.xunlei.com",
            "vod0227-h05-vip-lixian.xunlei.com",
            "vod0252-h05-vip-lixian.xunlei.com",
            "vod0253-h05-vip-lixian.xunlei.com",
            "vod0254-h05-vip-lixian.xunlei.com",
        ],
    ),
    (
        "b05",
        &[
            "vod0006-b05-vip-lixian.xunlei.com",
            "vod0009-b05-vip-lixian.xunlei.com",
            "vod0010-b05-vip-lixian.xunlei.com",
            "vod0011-b05-vip-lixian.xunlei.com",
            "vod0012-b05-vip-lixian.xunlei.com",
            "vod0013-b05-vip-lixian.xunlei.com",
            "vod0014-b05-vip-lixian.xunlei.com",
            "vod0043-b05-vip-lixian.xunlei.com",
            "vod0044-b05-vip-lixian.xunlei.com",
            "vod0045-b05-vip-lixian.xunlei.com",
            "vod0051-b05-vip-lixian.xunlei.com",
            "vod0053-b05-vip-lixian.xunlei.com",
            "vod0054-b05-vip-lixian.xunlei.com",
            "vod0055-b05-vip-lixian.xunlei.com",
            "vod0139-b05-vip-lixian.xunlei.com",
            "vod0140-b05-vip-lixian.xunlei.com",
            "vod0141-b05-vip-lixian.xunlei.com",
            "vod0142-b05-vip-lixian.xunlei.com",
            "vod0143-b05-vip-lixian.xunlei.com",
            "vod0349-b05-vip-lixian.xunlei.com",
        ],
    ),
    (
        "z01",
        &[
            "vod0032-z01-vip-lixian.xunlei.com",
            "vod0035-z01-vip-lixian.xunlei.com",
            "vod0036-z01-vip-lixian.xunlei.com",
            "vod0037-z01-vip-lixian.xunlei.com",
            "vod0038-z01-vip-lixian.xunlei.com",
            "vod0039-z01-vip-lixian.xunlei.com",
            "vod0040-z01-vip-lixian.xunlei.com",
            "vod0041-z01-vip-lixian.xunlei.com",
            "vod0042-z01-vip-lixian.xunlei.com",
            "vod0091-z01-vip-lixian.xunlei.com",
            "vod0093-z01-vip-lixian.xunlei.com",
            "vod0131-z01-vip-lixian.xunlei.com",
            "vod0135-z01-vip-lixian.xunlei.com",
            "vod0136-z01-vip-lixian.xunlei.com",
            "vod0146-z01-vip-lixian.xunlei.com",
            "vod0155-z01-vip-lixian.xunlei.com",
            "vod0156-z01-vip-lixian.xunlei.com",
            "vod0167-z01-vip-lixian.xunlei.com",
            "vod0195-z01-vip-lixian.xunlei.com",
            "vod0196-z01-vip-lixian.xunlei.com",
            "vod0281-z01-vip-lixian.xunlei.com",
        ],
    ),
    (
        "txyun08",
        &[
            "vod0064-txyun08-vip-lixian.xunlei.com",
            "vod0065-txyun08-vip-lixian.xunlei.com",
            "vod0066-txyun08-vip-lixian.xunlei.com",
            "vod0067-txyun08-vip-lixian.xunlei.com",
            "vod0068-txyun08-vip-lixian.xunlei.com",
            "vod0069-txyun08-vip-lixian.xunlei.com",
            "vod0070-txyun08-vip-lixian.xunlei.com",
            "vod0340-txyun08-vip-lixian.xunlei.com",
            "vod0341-txyun08-vip-lixian.xunlei.com",
        ],
    ),
    (
        "h01",
        &[
            "vod0070-h01-vip-lixian.xunlei.com",
            "vod0071-h01-vip-lixian.xunlei.com",
            "vod0074-h01-vip-lixian.xunlei.com",
            "vod0075-h01-vip-lixian.xunlei.com",
            "vod0131-h01-vip-lixian.xunlei.com",
            "vod0132-h01-vip-lixian.xunlei.com",
            "vod0153-h01-vip-lixian.xunlei.com",
        ],
    ),
    (
        "b02",
        &[
            "vod0080-b02-vip-lixian.xunlei.com",
            "vod0432-b02-vip-lixian.xunlei.com",
            "vod0531-b02-vip-lixian.xunlei.com",
            "vod0532-b02-vip-lixian.xunlei.com",
            "vod0533-b02-vip-lixian.xunlei.com",
            "vod0534-b02-vip-lixian.xunlei.com",
            "vod0537-b02-vip-lixian.xunlei.com",
            "vod0563-b02-vip-lixian.xunlei.com",
            "vod0565-b02-vip-lixian.xunlei.com",
            "vod0566-b02-vip-lixian.xunlei.com",
            "vod0568-b02-vip-lixian.xunlei.com",
            "vod0571-b02-vip-lixian.xunlei.com",
            "vod0572-b02-vip-lixian.xunlei.com",
            "vod0573-b02-vip-lixian.xunlei.com",
            "vod0595-b02-vip-lixian.xunlei.com",
            "vod0596-b02-vip-lixian.xunlei.com",
            "vod0597-b02-vip-lixian.xunlei.com",
            "vod0598-b02-vip-lixian.xunlei.com",
            "vod0636-b02-vip-lixian.xunlei.com",
            "vod0637-b02-vip-lixian.xunlei.com",
            "vod0638-b02-vip-lixian.xunlei.com",
            "vod0639-b02-vip-lixian.xunlei.com",
            "vod0640-b02-vip-lixian.xunlei.com",
            "vod0641-b02-vip-lixian.xunlei.com",
            "vod0642-b02-vip-lixian.xunlei.com",
            "vod0643-b02-vip-lixian.xunlei.com",
            "vod0644-b02-vip-lixian.xunlei.com",
            "vod0645-b02-vip-lixian.xunlei.com",
            "vod0646-b02-vip-lixian.xunlei.com",
            "vod0647-b02-vip-lixian.xunlei.com",
            "vod0648-b02-vip-lixian.xunlei.com",
            "vod0649-b02-vip-lixian.xunlei.com",
            "vod0650-b02-vip-lixian.xunlei.com",
            "vod0651-b02-vip-lixian.xunlei.com",
            "vod0652-b02-vip-lixian.xunlei.com",
            "vod0653-b02-vip-lixian.xunlei.com",
            "vod0654-b02-vip-lixian.xunlei.com",
            "vod0725-b02-vip-lixian.xunlei.com",
            "vod0726-b02-vip-lixian.xunlei.com",
            "vod0727-b02-vip-lixian.xunlei.com",
        ],
    ),
    (
        "h04",
        &[
            "vod0088-h04-vip-lixian.xunlei.com",
            "vod0089-h04-vip-lixian.xunlei.com",
            "vod0090-h04-vip-lixian.xunlei.com",
            "vod0091-h04-vip-lixian.xunlei.com",
            "vod0092-h04-vip-lixian.xunlei.com",
            "vod0093-h04-vip-lixian.xunlei.com",
            "vod0094-h04-vip-lixian.xunlei.com",
            "vod0097-h04-vip-lixian.xunlei.com",
            "vod0098-h04-vip-lixian.xunlei.com",
            "vod0099-h04-vip-lixian.xunlei.com",
            "vod0100-h04-vip-lixian.xunlei.com",
            "vod0101-h04-vip-lixian.xunlei.com",
            "vod0105-h04-vip-lixian.xunlei.com",
            "vod0128-h04-vip-lixian.xunlei.com",
            "vod0129-h04-vip-lixian.xunlei.com",
            "vod0143-h04-vip-lixian.xunlei.com",
            "vod0317-h04-vip-lixian.xunlei.com",
            "vod0318-h04-vip-lixian.xunlei.com",
            "vod0319-h04-vip-lixian.xunlei.com",
            "vod0320-h04-vip-lixian.xunlei.com",
        ],
    ),
    (
        "aliyun08",
        &[
            "vod0254-aliyun08-vip-lixian.xunlei.com",
            "vod0255-aliyun08-vip-lixian.xunlei.com",
            "vod0256-aliyun08-vip-lixian.xunlei.com",
            "vod0257-aliyun08-vip-lixian.xunlei.com",
            "vod0261-aliyun08-vip-lixian.xunlei.com",
            "vod0262-aliyun08-vip-lixian.xunlei.com",
            "vod0263-aliyun08-vip-lixian.xunlei.com",
            "vod0264-aliyun08-vip-lixian.xunlei.com",
            "vod0759-aliyun08-vip-lixian.xunlei.com",
            "vod0760-aliyun08-vip-lixian.xunlei.com",
            "vod9410-aliyun08-vip-lixian.xunlei.com",
            "vod9411-aliyun08-vip-lixian.xunlei.com",
            "vod9412-aliyun08-vip-lixian.xunlei.com",
        ],
    ),
    (
        "aliyun06",
        &[
            "vod0555-aliyun06-vip-lixian.xunlei.com",
            "vod0556-aliyun06-vip-lixian.xunlei.com",
            "vod1284-aliyun06-vip-lixian.xunlei.com",
            "vod1285-aliyun06-vip-lixian.xunlei.com",
            "vod1363-aliyun06-vip-lixian.xunlei.com",
            "vod1372-aliyun06-vip-lixian.xunlei.com",
            "vod1629-aliyun06-vip-lixian.xunlei.com",
            "vod1630-aliyun06-vip-lixian.xunlei.com",
            "vod1703-aliyun06-vip-lixian.xunlei.com",
            "vod1704-aliyun06-vip-lixian.xunlei.com",
            "vod1844-aliyun06-vip-lixian.xunlei.com",
        ],
    ),
    (
        "aliyun04",
        &[
            "vod0780-aliyun04-vip-lixian.xunlei.com",
            "vod0781-aliyun04-vip-lixian.xunlei.com",
            "vod3379-aliyun04-vip-lixian.xunlei.com",
            "vod3429-aliyun04-vip-lixian.xunlei.com",
            "vod3459-aliyun04-vip-lixian.xunlei.com",
            "vod3533-aliyun04-vip-lixian.xunlei.com",
            "vod4252-aliyun04-vip-lixian.xunlei.com",
            "vod4253-aliyun04-vip-lixian.xunlei.com",
            "vod4320-aliyun04-vip-lixian.xunlei.com",
            "vod4321-aliyun04-vip-lixian.xunlei.com",
        ],
    ),
];

/// 从 URL 抽取小写 host（去掉 scheme / userinfo / port）。
/// Python 侧用 urlparse().hostname：无 scheme 的裸主机名会被视为 path、hostname 为 None，
/// 故此处要求必须存在 "://" 才解析 host，否则返回 None（与 urlparse 语义一致）。
/// 同时 urlparse 会小写化 host 并剥离端口，此处等价处理。
/// 来源：toolkit/xunlei_url_classifier.py#classify_url#L66-L67 (parsed = urlparse(url); host = parsed.hostname or '')
fn host_of(url: &str) -> Option<String> {
    let after_scheme = match url.split_once("://") {
        Some((_, r)) => r,
        None => return None, // 无 scheme 视为裸主机名，urlparse().hostname 为 None → 非迅雷源
    };
    let authority = after_scheme.split('/').next().unwrap_or("");
    let hostport = match authority.rsplit_once('@') {
        Some((_, h)) => h,
        None => authority,
    };
    let host = hostport.split(':').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// 判断 host 是否属于迅雷自有基础设施（CDN / 已知域名 / PHub-SHub / 通配）。
fn is_xunlei_owned(host: &str) -> bool {
    if CDN_HOSTS.contains(&host) {
        return true;
    }
    for d in XUNLEI_DOMAINS {
        if host == *d || host.ends_with(&format!(".{}", d)) {
            return true;
        }
    }
    if host == "n0808.com" || host.ends_with(".n0808.com") {
        return true;
    }
    if host.ends_with(".sandai.net") || host.ends_with(".sandai.com") {
        return true;
    }
    if PHUB_SHUB_HOSTS.contains(&host) {
        return true;
    }
    false
}

/// 判定直链类别：A=迅雷自有 CDN（大概率账号绑定 / 高速），B=普通源。
/// 来源：toolkit/xunlei_url_classifier.py#classify_url#L55-L127
/// （本实现将 Python 的 A / PHUB 合并为 A，因 Rust 侧仅区分 A / B 两类）。
pub fn classify_url(url: &str) -> LinkClass {
    match host_of(url) {
        Some(host) if is_xunlei_owned(&host) => LinkClass::AThunderCdn,
        _ => LinkClass::BRegular,
    }
}

/// 按区域返回已知 CDN host 列表（从 Python 常量表完整移植）。
/// 来源：toolkit/xunlei_url_classifier.py#get_cdn_hosts_by_region#L130-L135 + xunlei_cdn_hosts.json#cdn_by_region
pub fn cdn_hosts_by_region() -> &'static [(&'static str, &'static [&'static str])] {
    CDN_BY_REGION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_regular_https_is_b() {
        assert_eq!(
            classify_url("https://example.com/path/file.mkv"),
            LinkClass::BRegular
        );
        assert_eq!(
            classify_url("http://speedtest.tele2.net/1MB.zip"),
            LinkClass::BRegular
        );
    }

    #[test]
    fn classify_cdn_positive_per_region() {
        // 每个区域至少取 1 个 host 作为正例
        for (region, hosts) in cdn_hosts_by_region() {
            let h = hosts[0];
            let url = format!("https://{}/video/file.mkv", h);
            assert_eq!(
                classify_url(&url),
                LinkClass::AThunderCdn,
                "region {} host {} 应判定为 A",
                region,
                h
            );
        }
    }

    #[test]
    fn classify_case_insensitive() {
        // 大小写容错：host 大写也应命中
        assert_eq!(
            classify_url("https://VOD0001-c01-vip-lixian.XUNLEI.com/path"),
            LinkClass::AThunderCdn
        );
    }

    #[test]
    fn classify_port_tolerant() {
        // 端口容错：带 :443 也应命中
        assert_eq!(
            classify_url("https://vod0001-c01-vip-lixian.xunlei.com:443/path?x=1"),
            LinkClass::AThunderCdn
        );
    }

    #[test]
    fn classify_n0808_wildcard() {
        assert_eq!(
            classify_url("https://foo.n0808.com/x"),
            LinkClass::AThunderCdn
        );
        assert_eq!(classify_url("https://n0808.com/x"), LinkClass::AThunderCdn);
    }

    #[test]
    fn classify_sandai_wildcard_and_phub() {
        assert_eq!(
            classify_url("https://hub5p.sandai.net/x"),
            LinkClass::AThunderCdn
        );
        assert_eq!(
            classify_url("https://viphub5pr.phub.sandai.net/x"),
            LinkClass::AThunderCdn
        );
    }

    #[test]
    fn classify_bare_host_without_scheme_is_b() {
        // 无 scheme 时无法解析出可信 host，按普通源处理
        assert_eq!(
            classify_url("vod0001-c01-vip-lixian.xunlei.com"),
            LinkClass::BRegular
        );
    }

    #[test]
    fn cdn_hosts_by_region_complete() {
        // 完整移植：12 个区域，且扁平集合数量与 JSON 中 cdn_count 一致(202)
        assert_eq!(cdn_hosts_by_region().len(), 12);
        let mut count = 0;
        for (_, hosts) in cdn_hosts_by_region() {
            count += hosts.len();
        }
        assert_eq!(count, 202);
        // 扁平 CDN_HOSTS 也应含 202 条
        assert_eq!(CDN_HOSTS.len(), 202);
    }
}
