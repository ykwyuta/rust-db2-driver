use rust_db2_driver::{Connection, Params};

#[test]
fn parses_url_and_default_port() {
    let p = Params::from_url("db2://alice:secret@localhost/SAMPLE").unwrap();
    assert_eq!(p.host, "localhost");
    assert_eq!(p.port, 50000);
    assert_eq!(p.database, "SAMPLE");
    assert_eq!(p.user, "alice");
    assert_eq!(p.password, "secret");
    let p = Params::from_url("db2://alice:a:b@db.example:25000/TESTDB").unwrap();
    assert_eq!(p.port, 25000);
    assert_eq!(p.password, "a:b");
}

#[test]
fn rejects_invalid_configuration_without_disclosing_credentials() {
    for url in [
        "postgres://alice:topsecret@localhost/DB",
        "db2://alice:topsecret@localhost",
        "db2://alice:topsecret@localhost:xyz/DB",
        "db2://alice:topsecret@localhost:0/DB",
        "db2://alice:topsecret@/DB",
        "db2://:topsecret@localhost/DB",
        "db2://alice:topsecret@localhost/",
        "db2://alice:topsecret@localhost/日本語",
    ] {
        let err = Params::from_url(url).unwrap_err();
        assert!(!err.to_string().contains("topsecret"));
    }
    let p = Params::from_url("db2://alice:topsecret@localhost/DB").unwrap();
    assert!(!format!("{p:?}").contains("topsecret"));
    let invalid = Params {
        user: "日本語".into(),
        ..p
    };
    // Validate structured parameters before touching the network or the encoder.
    assert!(
        Connection::connect(&invalid)
            .err()
            .unwrap()
            .0
            .contains("user")
    );
}
