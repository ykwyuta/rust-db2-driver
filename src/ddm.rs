//! DDM objects and DSS framing (DRDA Volume 3).
//!
//! A DSS (data stream structure) has a 6-byte header: length, X'D0',
//! format (chaining flags and type) and request correlator. It carries one
//! DDM object: length, code point and parameters, each again
//! length / code point / data.

/// Code points used by the client.
pub mod cp {
    pub const EXCSAT: u16 = 0x1041;
    pub const EXCSATRD: u16 = 0x1443;
    pub const EXTNAM: u16 = 0x115E;
    pub const SRVCLSNM: u16 = 0x1147;
    pub const SRVNAM: u16 = 0x116D;
    pub const SRVRLSLV: u16 = 0x115A;
    pub const MGRLVLLS: u16 = 0x1404;
    pub const AGENT: u16 = 0x1403;
    pub const SQLAM: u16 = 0x2407;
    pub const RDB: u16 = 0x240F;
    pub const SECMGR: u16 = 0x1440;
    pub const CMNTCPIP: u16 = 0x1474;
    pub const ACCSEC: u16 = 0x106D;
    pub const ACCSECRD: u16 = 0x14AC;
    pub const SECMEC: u16 = 0x11A2;
    pub const SECCHK: u16 = 0x106E;
    pub const SECCHKRM: u16 = 0x1219;
    pub const SECCHKCD: u16 = 0x11A4;
    pub const USRID: u16 = 0x11A0;
    pub const PASSWORD: u16 = 0x11A1;
    pub const ACCRDB: u16 = 0x2001;
    pub const ACCRDBRM: u16 = 0x2201;
    pub const RDBNAM: u16 = 0x2110;
    pub const RDBACCCL: u16 = 0x210F;
    pub const PRDID: u16 = 0x112E;
    pub const TYPDEFNAM: u16 = 0x002F;
    pub const TYPDEFOVR: u16 = 0x0035;
    pub const CCSIDSBC: u16 = 0x119C;
    pub const CCSIDDBC: u16 = 0x119D;
    pub const CCSIDMBC: u16 = 0x119E;
    pub const CRRTKN: u16 = 0x2135;
    pub const SVRCOD: u16 = 0x1149;
    pub const EXCSQLIMM: u16 = 0x200A;
    pub const EXCSQLSTT: u16 = 0x200B;
    pub const PRPSQLSTT: u16 = 0x200D;
    pub const OPNQRY: u16 = 0x200C;
    pub const CNTQRY: u16 = 0x2006;
    pub const CLSQRY: u16 = 0x2005;
    pub const RDBCMM: u16 = 0x200E;
    pub const RDBRLLBCK: u16 = 0x200F;
    pub const PKGNAMCSN: u16 = 0x2113;
    pub const SQLSTT: u16 = 0x2414;
    pub const SQLCARD: u16 = 0x2408;
    pub const SQLDARD: u16 = 0x2411;
    pub const QRYDSC: u16 = 0x241A;
    pub const QRYDTA: u16 = 0x241B;
    pub const QRYBLKSZ: u16 = 0x2114;
    pub const QRYINSID: u16 = 0x215B;
    pub const RTNSQLDA: u16 = 0x2116;
    pub const OPNQRYRM: u16 = 0x2205;
    pub const ENDQRYRM: u16 = 0x220B;
    pub const ENDUOWRM: u16 = 0x220C;
    pub const RDBUPDRM: u16 = 0x2218;
    pub const SQLERRRM: u16 = 0x2213;
    pub const RDBNACRM: u16 = 0x2204;
    pub const RDBNFNRM: u16 = 0x2211;
    pub const SYNTAXRM: u16 = 0x124C;
    pub const PRCCNVRM: u16 = 0x1245;
    pub const CMDCHKRM: u16 = 0x1254;
    pub const OBJNSPRM: u16 = 0x1253;
    pub const PRMNSPRM: u16 = 0x1251;
    pub const VALNSPRM: u16 = 0x1252;
    pub const CMDNSPRM: u16 = 0x1250;
    pub const MGRLVLRM: u16 = 0x1210;
    pub const SYNERRCD: u16 = 0x114A;
    pub const CODPNT: u16 = 0x000C;
}

/// DSS types (low nibble of the format byte).
pub const RQSDSS: u8 = 1;
pub const RPYDSS: u8 = 2;
pub const OBJDSS: u8 = 3;
/// Format flags.
pub const CHAINED: u8 = 0x40;
pub const SAME_CORRELATOR: u8 = 0x10;

/// A DDM object: code point and contents (parameters or data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub cp: u16,
    pub data: Vec<u8>,
}

impl Object {
    pub fn new(cp: u16) -> Object {
        Object {
            cp,
            data: Vec::new(),
        }
    }

    /// Adds a parameter.
    pub fn param(mut self, cp: u16, data: &[u8]) -> Object {
        self.data.extend(((data.len() + 4) as u16).to_be_bytes());
        self.data.extend(cp.to_be_bytes());
        self.data.extend(data);
        self
    }

    pub fn param_u16(self, cp: u16, v: u16) -> Object {
        self.param(cp, &v.to_be_bytes())
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = ((self.data.len() + 4) as u16).to_be_bytes().to_vec();
        out.extend(self.cp.to_be_bytes());
        out.extend(&self.data);
        out
    }

    /// The parameters (or nested objects) of the object.
    pub fn params(&self) -> Vec<Object> {
        parse_objects(&self.data)
    }

    pub fn get(&self, cp: u16) -> Option<Object> {
        self.params().into_iter().find(|p| p.cp == cp)
    }

    pub fn u16_of(&self, cp: u16) -> Option<u16> {
        let p = self.get(cp)?;
        Some(u16::from_be_bytes([*p.data.first()?, *p.data.get(1)?]))
    }
}

/// Objects laid out one after the other (with the 4-byte headers).
pub fn parse_objects(mut d: &[u8]) -> Vec<Object> {
    let mut out = Vec::new();
    while d.len() >= 4 {
        let len = u16::from_be_bytes([d[0], d[1]]) as usize;
        let cp = u16::from_be_bytes([d[2], d[3]]);
        // extended length (high bit): the length is in the next bytes
        let (hdr, len) = if len & 0x8000 != 0 {
            let n = len & 0x7FFF; // 4 + number of length bytes
            let k = n.saturating_sub(4);
            let mut l = 0usize;
            for i in 0..k {
                l = (l << 8) | d.get(4 + i).copied().unwrap_or(0) as usize;
            }
            (n, n + l)
        } else {
            (4, len)
        };
        if len < hdr || len > d.len() {
            break;
        }
        out.push(Object {
            cp,
            data: d[hdr..len].to_vec(),
        });
        d = &d[len..];
    }
    out
}

/// A DSS ready to send.
pub fn dss(format: u8, correlator: u16, obj: &Object) -> Vec<u8> {
    let body = obj.bytes();
    let mut out = ((body.len() + 6) as u16).to_be_bytes().to_vec();
    out.push(0xD0);
    out.push(format);
    out.extend(correlator.to_be_bytes());
    out.extend(body);
    out
}

/// A received DSS.
#[derive(Debug, Clone)]
pub struct Dss {
    pub format: u8,
    pub correlator: u16,
    pub object: Object,
}

impl Dss {
    pub fn chained(&self) -> bool {
        self.format & CHAINED != 0
    }
}

/// Reads one DSS (joining continuation segments).
pub fn read_dss(r: &mut impl std::io::Read) -> std::io::Result<Dss> {
    let mut h = [0u8; 6];
    r.read_exact(&mut h)?;
    if h[2] != 0xD0 {
        return Err(std::io::Error::other(format!("not a DSS: {h:02X?}")));
    }
    let mut len = u16::from_be_bytes([h[0], h[1]]) as usize;
    let mut body = Vec::new();
    let mut more = len & 0x8000 != 0;
    len &= 0x7FFF;
    let mut seg = vec![0u8; len.saturating_sub(6)];
    r.read_exact(&mut seg)?;
    body.extend(seg);
    while more {
        let mut l = [0u8; 2];
        r.read_exact(&mut l)?;
        let n = u16::from_be_bytes(l) as usize;
        more = n & 0x8000 != 0;
        let mut seg = vec![0u8; (n & 0x7FFF).saturating_sub(2)];
        r.read_exact(&mut seg)?;
        body.extend(seg);
    }
    let object = parse_objects(&body)
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("empty DSS"))?;
    Ok(Dss {
        format: h[3],
        correlator: u16::from_be_bytes([h[4], h[5]]),
        object,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_round_trip() {
        let o = Object::new(cp::ACCSEC)
            .param_u16(cp::SECMEC, 3)
            .param(cp::RDBNAM, b"DB");
        let b = dss(RQSDSS, 1, &o);
        assert_eq!(&b[..6], &[0, 22, 0xD0, 1, 0, 1]);
        let d = read_dss(&mut &b[..]).unwrap();
        assert_eq!(d.object, o);
        assert_eq!(d.object.u16_of(cp::SECMEC), Some(3));
        assert_eq!(d.object.get(cp::RDBNAM).unwrap().data, b"DB");
    }
}
