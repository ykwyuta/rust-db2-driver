//! Explicitly enabled with DB2_URL and --ignored; uses a unique table in the
//! connected user's schema and removes it on successful completion.
use rust_db2_driver::{Connection, Params, SqlCa, Value};

fn success(ca: SqlCa) {
    assert!(ca.sqlcode >= 0, "{ca:?}");
}

fn connection() -> Connection {
    let params = Params::from_url(&std::env::var("DB2_URL").expect("set DB2_URL")).unwrap();
    #[cfg(feature = "tls")]
    if let Some(ca) = std::env::var_os("DB2_TLS_CA") {
        use rust_db2_driver::rustls::{
            self,
            pki_types::{CertificateDer, pem::PemObject},
        };
        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_file_iter(ca).unwrap() {
            roots.add(certificate.unwrap()).unwrap();
        }
        let version = std::env::var("DB2_TLS_VERSION").unwrap_or_default();
        let versions: &[&rustls::SupportedProtocolVersion] = match version.as_str() {
            "" => rustls::DEFAULT_VERSIONS,
            "1.2" => &[&rustls::version::TLS12],
            "1.3" => &[&rustls::version::TLS13],
            _ => panic!("DB2_TLS_VERSION must be 1.2 or 1.3"),
        };
        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        return Connection::connect_with_tls(&params, std::sync::Arc::new(config)).unwrap();
    }
    Connection::connect(&params).unwrap()
}

#[test]
#[ignore = "requires DB2_URL and access to MON_GET_APPL_LOCKWAIT"]
fn cancels_a_running_lock_wait() {
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    let mut blocker = connection();
    let table = format!("RDBCANCEL{}", std::process::id());
    success(
        blocker
            .execute(&format!("CREATE TABLE {table} (ID INTEGER)"))
            .unwrap(),
    );
    success(
        blocker
            .execute(&format!("INSERT INTO {table} VALUES (1)"))
            .unwrap(),
    );
    success(blocker.commit().unwrap());
    let mut worker_connection = connection();
    let id = worker_connection
        .query("VALUES MON_GET_APPLICATION_HANDLE()")
        .unwrap();
    success(id.sqlca);
    let Value::Int(handle) = id.rows[0][0] else {
        panic!("missing application handle");
    };
    success(worker_connection.commit().unwrap());
    success(
        worker_connection
            .execute("SET CURRENT LOCK TIMEOUT 30")
            .unwrap(),
    );
    success(
        blocker
            .execute(&format!("LOCK TABLE {table} IN EXCLUSIVE MODE"))
            .unwrap(),
    );
    let cancel = worker_connection.cancel_handle();
    let (tx, rx) = mpsc::channel();
    let sql = format!("SELECT * FROM {table}");
    let worker = thread::spawn(move || {
        let result = worker_connection.query(&sql);
        tx.send(result).unwrap();
    });
    let mut monitor = connection();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_wait = false;
    while Instant::now() < deadline {
        let r = monitor
            .query(&format!(
                "SELECT COUNT(*) FROM TABLE(MON_GET_APPL_LOCKWAIT({handle}, -2)) AS W"
            ))
            .unwrap();
        success(r.sqlca);
        if r.rows[0][0] != Value::Int(0) {
            observed_wait = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    cancel.cancel().unwrap();
    let result = rx.recv_timeout(Duration::from_secs(2));
    // Release locks and remove the test object even if the assertions fail.
    success(blocker.rollback().unwrap());
    worker.join().unwrap();
    success(blocker.execute(&format!("DROP TABLE {table}")).unwrap());
    success(blocker.commit().unwrap());
    assert!(
        observed_wait,
        "server never reported the worker waiting on a lock"
    );
    assert!(result.unwrap().unwrap_err().is_cancelled());
}

#[test]
#[ignore = "requires DB2_URL pointing to a Db2 LUW test database"]
fn bound_values_and_statements() {
    let mut c = connection();
    let text = "こんにちは'); DROP TABLE anything; --";
    let values = [
        Value::Int(i64::MIN),
        Value::Str(text.into()),
        Value::Bytes(vec![0, 255]),
        Value::Dec("-1234.50".into()),
        Value::Float(1.5),
        Value::Null,
    ];
    let r = c.query_params("VALUES (CAST(? AS BIGINT), CAST(? AS VARCHAR(100)), CAST(? AS VARCHAR(2) FOR BIT DATA), CAST(? AS DECIMAL(9,2)), CAST(? AS DOUBLE), CAST(? AS INTEGER))", &values).unwrap();
    success(r.sqlca);
    assert_eq!(r.rows, vec![values.to_vec()]);
    let edge_values = [
        Value::Int(i64::MAX),
        Value::Str(String::new()),
        Value::Bytes(vec![]),
        Value::Dec("12345678901234567890123.12345678".into()),
    ];
    let r = c.query_params("VALUES (CAST(? AS BIGINT), CAST(? AS VARCHAR(10)), CAST(? AS VARCHAR(10) FOR BIT DATA), CAST(? AS DECIMAL(31,8)))", &edge_values).unwrap();
    success(r.sqlca);
    assert_eq!(r.rows, vec![edge_values.to_vec()]);
    // Exercise the FD:OCA continuation triplet after the 84th parameter.
    let markers = vec!["CAST(? AS INTEGER)"; 85].join(", ");
    let r = c
        .query_params(&format!("VALUES ({markers})"), &vec![Value::Null; 85])
        .unwrap();
    success(r.sqlca);
    assert_eq!(r.rows, vec![vec![Value::Null; 85]]);
    let table = format!("RDBBIND{}", std::process::id());
    success(
        c.execute(&format!(
            "CREATE TABLE {table} (ID BIGINT, NAME VARCHAR(100))"
        ))
        .unwrap(),
    );
    success(c.commit().unwrap());
    {
        let ca = c
            .execute_params(
                &format!("INSERT INTO {table} VALUES (?, ?)"),
                &[Value::Int(42), Value::Str(text.into())],
            )
            .unwrap();
        assert_eq!(ca.errd[2], 1);
        success(ca);
        let r = c
            .query_params(
                &format!("SELECT NAME FROM {table} WHERE ID = ?"),
                &[Value::Int(42)],
            )
            .unwrap();
        success(r.sqlca);
        assert_eq!(r.rows, vec![vec![Value::Str(text.into())]]);
        success(
            c.execute_params(
                &format!("UPDATE {table} SET NAME = ? WHERE ID = ?"),
                &[Value::Null, Value::Int(42)],
            )
            .unwrap(),
        );
        let r = c.query(&format!("SELECT NAME FROM {table}")).unwrap();
        success(r.sqlca);
        assert_eq!(r.rows, vec![vec![Value::Null]]);
        let bad = c
            .query_params("VALUES CAST(? AS BIGINT)", &[Value::Int(1), Value::Int(2)])
            .unwrap();
        assert!(bad.sqlca.sqlcode < 0, "{:?}", bad.sqlca);
        success(c.rollback().unwrap());
    }
    success(c.execute(&format!("DROP TABLE {table}")).unwrap());
    success(c.commit().unwrap());
}

#[test]
#[ignore = "requires DB2_URL pointing to a Db2 LUW test database"]
fn statements_queries_types_and_transactions() {
    let mut c = connection();
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
