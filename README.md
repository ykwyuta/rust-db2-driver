# rust-db2-driver

A synchronous, pure Rust Db2 driver using DRDA directly over TCP. It requires
no ODBC driver, DSN, IBM client library, or external Rust dependency.
Extracted from the working DRDA client in [Zeus](https://github.com/ykwyuta/Zeus);
see [NOTICE.md](NOTICE.md) for provenance. Licensed GPL-3.0-or-later.

## Use

Requires Rust 1.89 or later. Add the Git dependency (not yet published to crates.io):

```toml
[dependencies]
rust-db2-driver = { git = "https://github.com/ykwyuta/rust-db2-driver.git" }
```

```rust,no_run
use rust_db2_driver::{Connection, Params, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let params = Params::from_url(&std::env::var("DB2_URL")?)?;
    let mut conn = Connection::connect(&params)?;
    let result = conn.query("VALUES (42, CAST('hello' AS VARCHAR(20)))")?;
    if result.sqlca.sqlcode < 0 {
        return Err(format!("SQL error: {:?}", result.sqlca).into());
    }
    assert_eq!(result.rows[0][0], Value::Int(42));
    conn.rollback()?;
    Ok(())
}
```

Connection URLs have the form `db2://user:password@host:port/DATABASE`;
the port defaults to 50000. This is a simple connection string parser, not a
percent-decoding URL parser. For credentials containing delimiters, or an IPv6
address, construct `Params { host, port, database, user, password }` directly
(use an IPv6 address without brackets in `host`). DDM database/user/password
fields must be Latin-1; SQL text and ordinary character results use UTF-8.
`Params` debug output redacts the password; parse errors do not echo the URL.

For example, in PowerShell:

```powershell
$env:DB2_URL = 'db2://db2inst1:password@localhost:50000/SAMPLE'
cargo run --example query
cargo run --example query -- 'SELECT CURRENT DATE FROM SYSIBM.SYSDUMMY1'
```

## API and behavior

- `Connection::connect(&Params)` authenticates and opens a database session.
- `execute(&str)` runs DDL or DML and returns `SqlCa` (affected rows in `errd[2]`).
- `query(&str)` returns `Rows { columns, rows, sqlca }` and fetches all rows.
- `commit()` and `rollback()` explicitly end a transaction; there is no autocommit.
- `Value` represents NULL, signed integers, exact decimals as strings, floating
  point, text (including dates/times), and binary data.

I/O and protocol failures return `Err(Error)`. SQL failures return `Ok` with a
negative `sqlcode`; always inspect the SQLCA, including after commit/rollback.
Positive SQLCODEs are warnings. Column names can be empty because this query
description path does not request names. No Zeus runtime, COBOL types, or
environment variables are involved in the driver.

## Current scope

This initial driver targets Db2 for LUW (Community Edition). Db2 for z/OS and
IBM i compatibility has not been validated. It uses the server's existing
`NULLID.SYSSH200` dynamic SQL package; that package and appropriate database
permissions must be available.

Connections use unencrypted TCP and DRDA user/password authentication
(SECMEC 3). TLS and additional authentication methods are not implemented.
Use only on a trusted network or through a secured tunnel.

There is no parameter binding/prepared statement API, async I/O, connection
pool, configurable timeout, streaming cursor, or LOB/DECFLOAT support. Do not
concatenate untrusted input into SQL. Queries buffer their complete results;
unsupported column types return an error. This is an initial standalone port,
not a complete implementation of every Db2 type or DRDA feature.

`DB2_DRDA_TRACE=1` enables raw protocol diagnostics **after authentication**.
Traces can contain SQL and row data. Authentication requests are not traced
during `connect()`.

## Verification

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo test --doc
```

The Db2 integration test is explicitly ignored by default. Point `DB2_URL`
at a disposable Db2 LUW test database and run:

```sh
cargo test --test db2 -- --ignored --nocapture
```

It checks Unicode, numeric/date/binary values, NULL, multi-block result fetching,
SQL errors, and commit/rollback. It creates a unique table in the user's schema
and drops it on success; an interrupted or failed run may leave that test table.
The account needs CREATE TABLE, INSERT, SELECT, DELETE, and DROP permissions.
CI runs offline unit tests, documentation examples, and checks on Linux,
Windows, and macOS, plus a Rust 1.89 MSRV job. The real-server test is opt-in.
