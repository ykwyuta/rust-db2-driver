//! SQL statements over a DRDA connection: dynamic SQL in the sections of
//! the NULLID.SYSSH200 package that Db2 provides for dynamic SQL.

use crate::client::{Connection, Error, Result, ebc_padded, hex};
use crate::ddm::{Dss, Object, cp};

/// The SQL communication area returned by the server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SqlCa {
    pub sqlcode: i32,
    pub sqlstate: String,
    pub errproc: String,
    pub errd: [i32; 6],
    pub warn: String,
    /// Message tokens (SQLERRMC).
    pub tokens: Vec<String>,
}

impl SqlCa {
    pub fn ok() -> SqlCa {
        SqlCa {
            sqlstate: "00000".into(),
            ..SqlCa::default()
        }
    }
}

/// Reads an SQLCARD (SQLCAGRP); `le`: integers are little-endian.
pub fn parse_sqlca(d: &[u8], le: bool) -> SqlCa {
    parse_sqlca_len(d, le).0
}

/// An SQLCA and the bytes it takes (with its null indicator).
fn parse_sqlca_len(d: &[u8], le: bool) -> (SqlCa, usize) {
    if d.first() != Some(&0) {
        return (SqlCa::ok(), 1);
    }
    if d.len() < 19 {
        return (SqlCa::ok(), d.len());
    }
    let int = |b: &[u8]| -> i32 {
        let a = [b[0], b[1], b[2], b[3]];
        if le {
            i32::from_le_bytes(a)
        } else {
            i32::from_be_bytes(a)
        }
    };
    let s = |b: &[u8]| String::from_utf8_lossy(b).to_string();
    let mut ca = SqlCa {
        sqlcode: int(&d[1..5]),
        sqlstate: s(&d[5..10]),
        errproc: s(&d[10..18]),
        ..SqlCa::default()
    };
    // SQLCAXGRP
    let mut i = 18;
    if d.get(i) == Some(&0) {
        i += 1;
        for k in 0..6 {
            if i + 4 <= d.len() {
                ca.errd[k] = int(&d[i..i + 4]);
            }
            i += 4;
        }
        if i + 11 <= d.len() {
            ca.warn = s(&d[i..i + 11]);
        }
        i += 11;
        // SQLRDBNAME, SQLERRMSG_m, SQLERRMSG_s: 2-byte length + data
        let vc = |i: &mut usize| -> Vec<u8> {
            if *i + 2 > d.len() {
                return Vec::new();
            }
            let n = u16::from_be_bytes([d[*i], d[*i + 1]]) as usize;
            let v = d[*i + 2..(*i + 2 + n).min(d.len())].to_vec();
            *i += 2 + n;
            v
        };
        let _rdb = vc(&mut i);
        let m = vc(&mut i);
        let sm = vc(&mut i);
        let msg = if m.is_empty() { sm } else { m };
        ca.tokens = msg.split(|&b| b == 0xFF).map(s).collect();
    } else {
        i += 1;
    }
    (ca, i.min(d.len()))
}

/// A section of the dynamic SQL package.
pub(crate) fn pkgnamcsn(c: &Connection, section: u16) -> Vec<u8> {
    let mut v = c.rdbnam_param();
    v.extend(ebc_padded("NULLID", 18));
    v.extend(ebc_padded("SYSSH200", 18));
    // the consistency token is binary: these packages have "SYSLVL01"
    v.extend(b"SYSLVL01");
    v.extend(section.to_be_bytes());
    v
}

/// The SQLSTT object for a statement (UTF-8 text as the mixed string).
pub(crate) fn sqlstt(sql: &str) -> Object {
    let b = sql.as_bytes();
    let mut d = vec![0u8];
    d.extend((b.len() as u32).to_be_bytes());
    d.extend(b);
    d.push(0xFF);
    Object {
        cp: cp::SQLSTT,
        data: d,
    }
}

impl Connection {
    pub(crate) fn little_endian(&self) -> bool {
        self.server_typdef == "QTDSQLX86"
    }

    /// The SQLCA of a reply chain (SQLCARD or the SQLCA in an SQLDARD).
    pub(crate) fn reply_sqlca(&self, r: &[Dss]) -> SqlCa {
        for d in r {
            if d.object.cp == cp::SQLCARD || d.object.cp == cp::SQLDARD {
                return parse_sqlca(&d.object.data, self.little_endian());
            }
        }
        SqlCa::ok()
    }

    /// Executes a statement that returns no rows (EXCSQLIMM).
    pub fn execute(&mut self, sql: &str) -> Result<SqlCa> {
        let cmd = Object::new(cp::EXCSQLIMM).param(cp::PKGNAMCSN, &pkgnamcsn(self, 1));
        let r = self.exchange(&[(cmd, vec![sqlstt(sql)])])?;
        crate::client::check_errors(&r)?;
        Ok(self.reply_sqlca(&r))
    }

    pub fn commit(&mut self) -> Result<SqlCa> {
        let r = self.exchange(&[(Object::new(cp::RDBCMM), vec![])])?;
        crate::client::check_errors(&r)?;
        Ok(self.reply_sqlca(&r))
    }

    pub fn rollback(&mut self) -> Result<SqlCa> {
        let r = self.exchange(&[(Object::new(cp::RDBRLLBCK), vec![])])?;
        crate::client::check_errors(&r)?;
        Ok(self.reply_sqlca(&r))
    }

    /// Runs a query and returns all its rows (PRPSQLSTT, OPNQRY, then
    /// CNTQRY until the end of the data).
    pub fn query(&mut self, sql: &str) -> Result<Rows> {
        self.section = if self.section >= 65 || self.section < 2 {
            2
        } else {
            self.section + 1
        };
        let pkg = pkgnamcsn(self, self.section);
        let prp = Object::new(cp::PRPSQLSTT)
            .param(cp::PKGNAMCSN, &pkg)
            .param(cp::RTNSQLDA, &[0xF1]);
        let r = self.exchange(&[(prp, vec![sqlstt(sql)])])?;
        crate::client::check_errors(&r)?;
        let ca = self.reply_sqlca(&r);
        if ca.sqlcode < 0 {
            return Ok(Rows {
                sqlca: ca,
                ..Rows::default()
            });
        }
        let opn = Object::new(cp::OPNQRY)
            .param(cp::PKGNAMCSN, &pkg)
            .param(cp::QRYBLKSZ, &QRYBLKSZ.to_be_bytes());
        let mut r = self.exchange(&[(opn, vec![])])?;
        crate::client::check_errors(&r)?;
        let mut rows = Rows::default();
        let Some(rm) = r.iter().find(|d| d.object.cp == cp::OPNQRYRM) else {
            rows.sqlca = self.reply_sqlca(&r);
            return Ok(rows);
        };
        let insid = rm.object.get(cp::QRYINSID).map(|o| o.data);
        let result = (|| {
            let Some(dsc) = r.iter().find(|d| d.object.cp == cp::QRYDSC) else {
                return Err(Error("OPNQRY reply without QRYDSC".into()));
            };
            rows.columns = parse_qrydsc(&dsc.object.data)?;
            let mut data = Vec::new();
            loop {
                for d in &r {
                    if d.object.cp == cp::QRYDTA {
                        data.extend(&d.object.data);
                    }
                }
                let warning = std::mem::take(&mut rows.sqlca);
                let (done, ca) = self.decode_rows(&data, &mut rows)?;
                if rows.sqlca.sqlcode == 0 && warning.sqlcode > 0 {
                    rows.sqlca = warning;
                }
                if done {
                    if ca.sqlcode != 100 || rows.sqlca.sqlcode <= 0 {
                        rows.sqlca = ca;
                    }
                    break;
                }
                if r.iter().any(|d| d.object.cp == cp::ENDQRYRM) {
                    rows.sqlca = self.reply_sqlca(&r);
                    break;
                }
                // more rows: continue the query
                let mut c = Object::new(cp::CNTQRY)
                    .param(cp::PKGNAMCSN, &pkg)
                    .param(cp::QRYBLKSZ, &QRYBLKSZ.to_be_bytes());
                if let Some(id) = &insid {
                    c = c.param(cp::QRYINSID, id);
                }
                r = self.exchange(&[(c, vec![])])?;
                crate::client::check_errors(&r)?;
                if !r.iter().any(|d| d.object.cp == cp::QRYDTA) {
                    rows.sqlca = self.reply_sqlca(&r);
                    break;
                }
            }
            if rows.sqlca.sqlcode == 100 {
                rows.sqlca = SqlCa::ok();
            }
            Ok(rows)
        })();
        // SYSSH200 cursors can remain open after the end-of-data SQLCA.
        // Release them before returning buffered rows, including decoding errors.
        if !r.iter().any(|d| d.object.cp == cp::ENDQRYRM) {
            let mut close = Object::new(cp::CLSQRY).param(cp::PKGNAMCSN, &pkg);
            if let Some(id) = &insid {
                close = close.param(cp::QRYINSID, id);
            }
            let closed = self.exchange(&[(close, vec![])]).and_then(|reply| {
                crate::client::check_errors(&reply)?;
                let ca = self.reply_sqlca(&reply);
                if ca.sqlcode < 0 {
                    return Err(Error(format!("close query failed: {ca:?}")));
                }
                Ok(())
            });
            if result.is_ok() {
                closed?;
            }
        }
        result
    }

    /// Decodes the complete rows in `data` (from the start, replacing what
    /// was decoded before); true when the end of the data was reached.
    fn decode_rows(&self, data: &[u8], rows: &mut Rows) -> Result<(bool, SqlCa)> {
        let le = self.little_endian();
        rows.rows.clear();
        let mut i = 0;
        while i < data.len() {
            match data[i] {
                // an SQLCA for the row: the end of the data, an error, or
                // a warning on the row that follows
                0x00 => {
                    let (ca, n) = parse_sqlca_len(&data[i..], le);
                    if ca.sqlcode == 100 || ca.sqlcode < 0 {
                        return Ok((true, ca));
                    }
                    i += n;
                    // a null SQLDIAGGRP
                    if data.get(i) == Some(&0xFF) && data.get(i + 1) == Some(&0) {
                        i += 1;
                    }
                    if rows.sqlca.sqlcode == 0 {
                        rows.sqlca = ca;
                    }
                }
                0xFF => i += 1,
                b => return Err(Error(format!("unexpected row indicator {b:02X}"))),
            }
            // the row (group) is present
            if data.get(i) != Some(&0) {
                i += 1;
                continue;
            }
            i += 1;
            let mut row = Vec::with_capacity(rows.columns.len());
            for c in &rows.columns {
                if c.nullable {
                    // a row continued in the next query block
                    let Some(&ind) = data.get(i) else {
                        return Ok((false, SqlCa::ok()));
                    };
                    i += 1;
                    if ind != 0 {
                        row.push(Value::Null);
                        continue;
                    }
                }
                match decode_value(c, data.get(i..).unwrap_or(&[]), le) {
                    Ok((v, n)) => {
                        row.push(v);
                        i += n;
                    }
                    Err(e) if e.0 == TRUNCATED => return Ok((false, SqlCa::ok())),
                    Err(e) => return Err(e),
                }
            }
            rows.rows.push(row);
        }
        Ok((false, SqlCa::ok()))
    }
}

/// The error of a row whose data continues in the next query block.
const TRUNCATED: &str = "row data truncated";

/// Query block size requested (bytes).
const QRYBLKSZ: u32 = 32767;

/// A column of a query result (FD:OCA description).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// DRDA data type (the nullable form is the odd code).
    pub drda_type: u8,
    pub nullable: bool,
    /// Length; for DECIMAL precision (high byte) and scale (low byte).
    pub len: u16,
    /// The column name, when the server describes it (else empty).
    pub name: String,
}

/// A column value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    /// A decimal number as text: `-1234.50`.
    Dec(String),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rows {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub sqlca: SqlCa,
}

/// The column descriptions of a QRYDSC: its X'76' triplets list the
/// columns as (type, length) entries.
fn parse_qrydsc(d: &[u8]) -> Result<Vec<Column>> {
    let mut cols = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        let len = d[i] as usize;
        if len < 3 || i + len > d.len() {
            break;
        }
        if d[i + 1] == 0x76 {
            let mut k = i + 3;
            while k + 3 <= i + len {
                let t = d[k];
                cols.push(Column {
                    drda_type: t & 0xFE,
                    nullable: t & 1 == 1,
                    len: u16::from_be_bytes([d[k + 1], d[k + 2]]),
                    name: String::new(),
                });
                k += 3;
            }
        }
        i += len;
    }
    if cols.is_empty() {
        return Err(Error(format!("no columns in QRYDSC {}", hex(d))));
    }
    Ok(cols)
}

fn packed_text(b: &[u8], scale: usize) -> String {
    let mut digits = String::new();
    for (k, &x) in b.iter().enumerate() {
        digits.push((b'0' + (x >> 4)) as char);
        if k + 1 < b.len() {
            digits.push((b'0' + (x & 0x0F)) as char);
        }
    }
    let neg = matches!(b.last().map(|x| x & 0x0F), Some(0x0D | 0x0B));
    let digits = digits.trim_start_matches('0');
    let digits = format!("{digits:0>width$}", width = scale + 1);
    let (int, frac) = digits.split_at(digits.len() - scale);
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.push_str(int);
    if scale > 0 {
        s.push('.');
        s.push_str(frac);
    }
    s
}

/// Decodes one value; returns it and the bytes used.
fn decode_value(c: &Column, d: &[u8], le: bool) -> Result<(Value, usize)> {
    let need = |n: usize| -> Result<()> {
        if d.len() < n {
            Err(Error(TRUNCATED.into()))
        } else {
            Ok(())
        }
    };
    let int = |n: usize| -> i64 {
        let mut b = [0u8; 8];
        if le {
            b[..n].copy_from_slice(&d[..n]);
            let v = i64::from_le_bytes(b);
            let shift = 64 - 8 * n as u32;
            (v << shift) >> shift
        } else {
            b[8 - n..].copy_from_slice(&d[..n]);
            let v = i64::from_be_bytes(b);
            let shift = 64 - 8 * n as u32;
            (v << shift) >> shift
        }
    };
    let text = |b: &[u8]| String::from_utf8_lossy(b).to_string();
    Ok(match c.drda_type {
        0x02 => {
            need(4)?;
            (Value::Int(int(4)), 4)
        }
        0x04 => {
            need(2)?;
            (Value::Int(int(2)), 2)
        }
        0x16 => {
            need(8)?;
            (Value::Int(int(8)), 8)
        }
        0x0E => {
            let p = (c.len >> 8) as usize;
            let s = (c.len & 0xFF) as usize;
            let n = p / 2 + 1;
            need(n)?;
            (Value::Dec(packed_text(&d[..n], s)), n)
        }
        0x0A => {
            need(8)?;
            let b = [d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]];
            let v = if le {
                f64::from_le_bytes(b)
            } else {
                f64::from_be_bytes(b)
            };
            (Value::Float(v), 8)
        }
        0x0C => {
            need(4)?;
            let b = [d[0], d[1], d[2], d[3]];
            let v = if le {
                f32::from_le_bytes(b)
            } else {
                f32::from_be_bytes(b)
            };
            (Value::Float(v as f64), 4)
        }
        // fixed-length character, dates and times
        0x30 | 0x3C | 0x20 | 0x22 | 0x24 => {
            let n = c.len as usize;
            need(n)?;
            (Value::Str(text(&d[..n])), n)
        }
        0x26 => {
            let n = c.len as usize;
            need(n)?;
            (Value::Bytes(d[..n].to_vec()), n)
        }
        // varying-length character / binary: 2-byte length
        0x32 | 0x3E | 0x28 | 0x34 | 0x40 => {
            need(2)?;
            let n = u16::from_be_bytes([d[0], d[1]]) as usize;
            need(2 + n)?;
            let v = if c.drda_type == 0x28 {
                Value::Bytes(d[2..2 + n].to_vec())
            } else {
                Value::Str(text(&d[2..2 + n]))
            };
            (v, 2 + n)
        }
        t => {
            return Err(Error(format!(
                "column type X'{t:02X}' (length {}) is not supported yet",
                c.len
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_decimals() {
        assert_eq!(packed_text(&[0x00, 0x00, 0x01, 0x23, 0x4C], 2), "12.34");
        assert_eq!(packed_text(&[0x00, 0x00, 0x00, 0x50, 0x0D], 2), "-5.00");
        assert_eq!(packed_text(&[0x0C], 0), "0");
    }

    #[test]
    fn query_description() {
        let d = [
            0x18, 0x76, 0xD0, 0x02, 0x00, 0x04, 0x3F, 0x00, 0x14, 0x0F, 0x09, 0x02, 0x3D, 0x00,
            0x03, 0x21, 0x00, 0x0A, 0x05, 0x00, 0x02, 0x17, 0x00, 0x08, 0x09, 0x71, 0xE0, 0x54,
            0x00, 0x01, 0xD0, 0x00, 0x01,
        ];
        let c = parse_qrydsc(&d).unwrap();
        assert_eq!(c.len(), 7);
        assert_eq!(
            c[2],
            Column {
                drda_type: 0x0E,
                nullable: true,
                len: 0x0902,
                name: String::new(),
            }
        );
    }
}
