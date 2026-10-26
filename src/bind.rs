//! Encoding input values as DRDA SQLDTA (FD:OCA descriptor and row data).

use crate::{
    Error, Result, Value,
    ddm::{Object, cp},
};

pub(crate) fn parameter_data(params: &[Value]) -> Result<Vec<Object>> {
    if params.is_empty() {
        return Ok(vec![]);
    }
    let mut types = Vec::new();
    let mut row = vec![0]; // SQLDTAGRP is present.
    for value in params {
        row.push(if matches!(value, Value::Null) {
            0xff
        } else {
            0
        });
        let (kind, len) = match value {
            Value::Null => (0x3f, 1), // Nullable UTF-8 VARCHAR, coerced by Db2.
            Value::Int(n) => {
                row.extend(n.to_be_bytes());
                (0x17, 8)
            }
            Value::Float(n) => {
                if !n.is_finite() {
                    return Err(Error("non-finite floating-point parameter".into()));
                }
                row.extend(n.to_be_bytes());
                (0x0b, 8)
            }
            Value::Str(s) => (0x3f, varying(&mut row, s.as_bytes())?),
            Value::Bytes(b) => (0x29, varying(&mut row, b)?),
            Value::Dec(s) => {
                let (precision, scale, packed) = decimal(s)?;
                row.extend(packed);
                (0x0f, u16::from(precision) << 8 | u16::from(scale))
            }
        };
        types.push((kind, len));
        // No outbound continuation segments or LOB parameters yet.
        if row.len() + 3 * types.len() + 6 + 3 * types.len().div_ceil(84) + 8 > 32757 {
            return Err(Error(
                "bound parameters exceed the SQLDTA size limit".into(),
            ));
        }
    }
    let mut descriptor = Vec::new();
    for (i, chunk) in types.chunks(84).enumerate() {
        descriptor.extend([
            (3 + 3 * chunk.len()) as u8,
            if i == 0 { 0x76 } else { 0x7f },
            if i == 0 { 0xd0 } else { 0 },
        ]);
        for &(kind, len) in chunk {
            descriptor.push(kind);
            descriptor.extend(len.to_be_bytes());
        }
    }
    // SQLDTA late row descriptor: one instance of SQLDTAGRP.
    descriptor.extend([6, 0x71, 0xe4, 0xd0, 0, 1]);
    let mut data = Object {
        cp: cp::FDODSC,
        data: descriptor,
    }
    .bytes();
    data.extend(
        Object {
            cp: cp::FDODTA,
            data: row,
        }
        .bytes(),
    );
    Ok(vec![Object {
        cp: cp::SQLDTA,
        data,
    }])
}

fn varying(row: &mut Vec<u8>, bytes: &[u8]) -> Result<u16> {
    let len = u16::try_from(bytes.len())
        .ok()
        .filter(|n| *n <= 32767)
        .ok_or_else(|| Error("text/binary parameter exceeds 32767 bytes".into()))?;
    row.extend(len.to_be_bytes());
    row.extend(bytes);
    Ok(len.max(1))
}

fn decimal(s: &str) -> Result<(u8, u8, Vec<u8>)> {
    let invalid = || {
        Error(
            "decimal parameter must have 1..31 digits and an optional sign and decimal point"
                .into(),
        )
    };
    let negative = s.starts_with('-');
    let unsigned = s.strip_prefix(['-', '+']).unwrap_or(s);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let digits: Vec<u8> = whole
        .bytes()
        .chain(fraction.bytes())
        .map(|b| b - b'0')
        .collect();
    if digits.len() > 31 {
        return Err(invalid());
    }
    let precision = digits.len() as u8;
    let scale = fraction.len() as u8;
    let mut nibbles = Vec::new();
    if digits.len().is_multiple_of(2) {
        nibbles.push(0);
    }
    nibbles.extend(digits);
    nibbles.push(if negative { 0x0d } else { 0x0c });
    let packed = nibbles
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| pair[0] << 4 | pair[1])
        .collect();
    Ok((precision, scale, packed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_wire_data_and_no_interpolation() {
        let text = "こんにちは'); DROP TABLE X; --";
        let data = parameter_data(&[
            Value::Int(-42),
            Value::Str(text.into()),
            Value::Bytes(vec![0, 255]),
            Value::Null,
            Value::Float(1.5),
            Value::Dec("-12.34".into()),
        ])
        .unwrap();
        assert_eq!(data[0].cp, cp::SQLDTA);
        let objects = data[0].params();
        assert_eq!(objects[0].cp, cp::FDODSC);
        assert_eq!(
            objects[0].data,
            [
                21,
                0x76,
                0xd0,
                0x17,
                0,
                8,
                0x3f,
                0,
                text.len() as u8,
                0x29,
                0,
                2,
                0x3f,
                0,
                1,
                0x0b,
                0,
                8,
                0x0f,
                4,
                2,
                6,
                0x71,
                0xe4,
                0xd0,
                0,
                1
            ]
        );
        let mut expected = vec![0, 0];
        expected.extend((-42i64).to_be_bytes());
        expected.push(0);
        expected.extend((text.len() as u16).to_be_bytes());
        expected.extend(text.as_bytes());
        expected.extend([0, 0, 2, 0, 255, 255, 0]);
        expected.extend(1.5f64.to_be_bytes());
        expected.extend([0, 0x01, 0x23, 0x4d]);
        assert_eq!(
            objects[1],
            Object {
                cp: cp::FDODTA,
                data: expected
            }
        );
    }

    #[test]
    fn decimal_validation_and_packing() {
        assert_eq!(decimal("0").unwrap(), (1, 0, vec![0x0c]));
        assert_eq!(decimal("+.50").unwrap(), (2, 2, vec![0x05, 0x0c]));
        assert_eq!(decimal("123").unwrap(), (3, 0, vec![0x12, 0x3c]));
        for bad in [
            "",
            "-",
            ".",
            "1e2",
            "NaN",
            "1.2.3",
            " 1",
            "１２",
            "12345678901234567890123456789012",
        ] {
            assert!(decimal(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn continuation_descriptors_and_limits() {
        let data = parameter_data(&vec![Value::Null; 85]).unwrap();
        let descriptor = &data[0].params()[0].data;
        assert_eq!(&descriptor[..3], &[255, 0x76, 0xd0]);
        assert_eq!(&descriptor[255..261], &[6, 0x7f, 0, 0x3f, 0, 1]);
        assert!(parameter_data(&[Value::Str("x".repeat(32767))]).is_err());
        assert!(parameter_data(&[Value::Bytes(vec![0; 65536])]).is_err());
        assert!(parameter_data(&[Value::Float(f64::NAN)]).is_err());
        assert!(parameter_data(&[Value::Float(f64::INFINITY)]).is_err());
        assert!(parameter_data(&[]).unwrap().is_empty());
    }
}
