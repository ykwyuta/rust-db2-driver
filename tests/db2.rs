//! Explicitly enabled with DB2_URL and --ignored; uses a unique table in the
//! connected user's schema and removes it on successful completion.
use rust_db2_driver::{Connection, Params, SqlCa, Value};

fn success(ca: SqlCa) {
    assert!(ca.sqlcode >= 0, "{ca:?}");
}

#[test]
#[ignore = "requires DB2_URL pointing to a Db2 LUW test database"]
fn statements_queries_types_and_transactions() {
    let url = std::env::var("DB2_URL").expect("set DB2_URL to a Db2 LUW test database");
    let params = Params::from_url(&url).unwrap();
    let mut c = Connection::connect(&params).unwrap();
    let table = format!(
        "RDB{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let r = c.query("VALUES (CAST('こんにちは' AS VARCHAR(40)), CAST(-12 AS SMALLINT), CAST(12345678901 AS BIGINT), CAST(1.5 AS DOUBLE), DATE('2026-10-26'), CAST(X'00FF' AS VARCHAR(2) FOR BIT DATA))").unwrap();
    success(r.sqlca);
    assert_eq!(
        r.rows[0],
        vec![
            Value::Str("こんにちは".into()),
            Value::Int(-12),
            Value::Int(12345678901),
            Value::Float(1.5),
            Value::Str("2026-10-26".into()),
            Value::Bytes(vec![0, 255])
        ]
    );
    success(c.execute(&format!("CREATE TABLE {table} (ID INTEGER NOT NULL, NAME VARCHAR(20), AMT DECIMAL(9,2), CD CHAR(3))")).unwrap());
    // Enough data for the query to span multiple DRDA response blocks.
    for i in 1..=2500 {
        let ca = c
            .execute(&format!(
                "INSERT INTO {table} VALUES ({i}, 'N{i}', {i}.25, NULL)"
            ))
            .unwrap();
        assert_eq!(ca.errd[2], 1, "{ca:?}");
        success(ca);
    }
    success(c.commit().unwrap());
    let r = c
        .query(&format!(
            "SELECT ID, NAME, AMT, CD FROM {table} ORDER BY ID"
        ))
        .unwrap();
    success(r.sqlca);
    assert_eq!(r.rows.len(), 2500);
    assert_eq!(
        r.rows[2499],
        vec![
            Value::Int(2500),
            Value::Str("N2500".into()),
            Value::Dec("2500.25".into()),
            Value::Null
        ]
    );
    let ca = c
        .execute(&format!("INSERT INTO {table} VALUES ('X')"))
        .unwrap();
    assert!(ca.sqlcode < 0);
    let r = c.query(&format!("SELECT * FROM {table}_MISSING")).unwrap();
    assert_eq!(r.sqlca.sqlcode, -204);
    success(c.execute(&format!("DELETE FROM {table}")).unwrap());
    success(c.rollback().unwrap());
    let r = c.query(&format!("SELECT COUNT(*) FROM {table}")).unwrap();
    success(r.sqlca);
    assert_eq!(r.rows[0][0], Value::Int(2500));
    // Reuse package sections after the driver's section number wraps.
    for i in 0..70 {
        let r = c.query(&format!("VALUES {i}")).unwrap();
        success(r.sqlca);
        assert_eq!(r.rows[0][0], Value::Int(i));
    }
    success(c.execute(&format!("DROP TABLE {table}")).unwrap());
    success(c.commit().unwrap());
}
