# rust-db2-driver

A synchronous, pure Rust Db2 driver using DRDA directly over TCP or TLS. It
requires no ODBC driver, DSN, or IBM client library. TLS uses rustls and is
enabled by default; `default-features = false` removes runtime dependencies.
Extracted from the working DRDA client in [Zeus](https://github.com/ykwyuta/Zeus);
see [NOTICE.md](NOTICE.md) for provenance. Licensed under the MIT License.

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
- `execute_params(&str, &[Value])` and `query_params(&str, &[Value])` bind
  positional `?` markers using DRDA SQLDTA, with no SQL interpolation.
- `connect_tls(&Params)` verifies the server certificate and hostname;
  `connect_with_tls(&Params, Arc<rustls::ClientConfig>)` supports private CAs.
- `cancel_handle()` returns a cloneable handle usable from another thread.
- `commit()` and `rollback()` explicitly end a transaction; there is no autocommit.
- `Value` represents NULL, signed integers, exact decimals as strings, floating
  point, text (including dates/times), and binary data.

I/O and protocol failures return `Err(Error)`. SQL failures return `Ok` with a
negative `sqlcode`; always inspect the SQLCA, including after commit/rollback.
Positive SQLCODEs are warnings. Column names can be empty because this query
description path does not request names. No Zeus runtime or COBOL types are
required by the driver.

## TLS, parameters, and cancellation

`connect()` retains the existing plain TCP behavior. For TLS, specify the
server's TLS port in `Params`, then call `Connection::connect_tls(&params)`.
It verifies the certificate against public roots and the host in `Params`;
there is no automatic fallback to plain TCP. For a private CA, load its PEM
certificate into a rustls root store (rustls is re-exported by this crate):

```rust,no_run
use std::sync::Arc;
use rust_db2_driver::{Connection, Params, Value, rustls};
use rustls::pki_types::{CertificateDer, pem::PemObject};

fn main() -> Result<(), Box<dyn std::error::Error>> {
let params = Params::from_url(&std::env::var("DB2_URL")?)?;
let mut roots = rustls::RootCertStore::empty();
for cert in CertificateDer::pem_file_iter("db2-ca.pem")? {
    roots.add(cert?)?;
}
let config = rustls::ClientConfig::builder_with_provider(Arc::new(
    rustls::crypto::ring::default_provider(),
))
.with_safe_default_protocol_versions()?
.with_root_certificates(roots)
.with_no_client_auth();
let mut conn = Connection::connect_with_tls(&params, Arc::new(config))?;
let rows = conn.query_params(
    "VALUES (CAST(? AS BIGINT), CAST(? AS VARCHAR(100)))",
    &[Value::Int(42), Value::Str("こんにちは' ? --".into())],
)?;
assert!(rows.sqlca.sqlcode >= 0);
conn.rollback()?;
Ok(())
}
```

Input values support BIGINT, finite DOUBLE, UTF-8 VARCHAR, VARBINARY, packed
DECIMAL (up to 31 digits), and NULL. `Value::Null` is transmitted as a nullable
VARCHAR and converted by Db2 to the target type. Use explicit `CAST(? AS ...)`
when Db2 cannot infer a marker's SQL type. Marker count/type errors are returned
in the SQLCA. Each call prepares and executes a statement; there is no reusable
prepared-statement object. Outbound continuation/LOB parameters are not yet
supported: SQLDTA payloads over 32757 bytes return an error before execution.
Parameter markers represent values, not identifiers such as table names.

To cancel a blocking call, obtain a handle before moving the connection:

```rust,no_run
use rust_db2_driver::{Connection, Params};
fn main() -> Result<(), Box<dyn std::error::Error>> {
let params = Params::from_url(&std::env::var("DB2_URL")?)?;
let mut conn = Connection::connect(&params)?;
let cancel = conn.cancel_handle();
let worker = std::thread::spawn(move || conn.query("SELECT * FROM BIG_TABLE"));
// From a UI event handler or another thread when cancellation is requested:
cancel.cancel()?;
let result = worker.join().expect("query worker panicked");
if let Err(error) = result {
    assert!(error.is_cancelled());
}
Ok(())
}
```

Cancellation shuts down this connection's underlying socket, including for
TLS, and interrupts blocked I/O. It is permanent and idempotent, even if the
connection is idle. Reconnect afterwards. This is connection abort, not a Db2
statement-cancel protocol; it does not guarantee immediate server termination.
Transactions are left to Db2's disconnect handling. In particular, cancellation
while committing does not determine whether the commit completed. A call that
already completed may return its result before a concurrent cancellation.

## Explain access plans

The existing `execute` and `query` API can retrieve Db2 LUW access plans;
no ODBC or external `db2exfmt` command is needed. Run the complete demo against
a **development database**:

```sh
cargo run --example explain
```

The demo creates a unique schema with a 1,000-row table, prepares Explain
tables if missing, issues `EXPLAIN PLAN SET QUERYTAG = '...' FOR SELECT ...`,
and queries `EXPLAIN_OPERATOR`, `EXPLAIN_STREAM`, and `EXPLAIN_STATEMENT`.
A unique query tag and the complete statement key isolate this plan from others.
It reports operator types, cumulative estimated costs (timerons), connections
between operators, estimated row counts, accessed objects, and SQL text.
`STATEMENT_TEXT` is a CLOB, so the example explicitly casts it to VARCHAR(4000);
longer statements are truncated in this display.

Verified using this driver on Db2 LUW Community Edition 12.1.5.0:

```text
SELECT NAME FROM <demo schema>.DEMO WHERE ID = 42

operator   cumulative estimated cost
RETURN     13.533363342285156
FETCH      13.533363342285156
IXSCAN      6.767663478851318

IXSCAN -> FETCH: estimated 1 row
FETCH -> RETURN: estimated 1 row
```

These are optimizer estimates, not measured execution times. This verifies
SQL-based EXPLAIN retrieval, not runtime section actuals or every Explain tool.
Costs and chosen operators can vary with statistics and database settings.

Db2 looks for dynamic SQL's Explain tables under the **session authorization
ID**, then SYSTOOLS; changing CURRENT SCHEMA does not select a different set.
The demo creates missing Explain tables under the session authorization ID via
`SYSPROC.SYSINSTALLOBJECTS`. It requires privileges to execute that procedure,
create the demo objects, and populate/read Explain tables. On exit it rolls back
its captured plan and removes objects it created, preserving pre-existing
Explain tables. An interrupted run may need manual cleanup.

References: [IBM Explain tables](https://www.ibm.com/docs/en/db2-as-a-service?topic=reference-explain-tables),
[EXPLAIN statement](https://www.ibm.com/docs/en/db2/12.1.x?topic=statements-explain),
[SYSINSTALLOBJECTS](https://www.ibm.com/docs/en/db2/12.1.0?topic=views-sysinstallobjects-create-drop-objects).

## Current scope

This initial driver targets Db2 for LUW (Community Edition). Db2 for z/OS and
IBM i compatibility has not been validated. It uses the server's existing
`NULLID.SYSSH200` dynamic SQL package; that package and appropriate database
permissions must be available.

Connections use DRDA user/password authentication (SECMEC 3). Use a TLS
connection on untrusted networks. Additional authentication methods are not
implemented. DRDA control strings use CCSID 500, including password punctuation.

There is no async I/O, connection pool, configurable timeout, streaming cursor,
or LOB/DECFLOAT support. Use parameter binding for untrusted values.
Queries buffer their complete results;
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
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test --no-default-features --all-targets
```

The Db2 integration test is explicitly ignored by default. Point `DB2_URL`
at a disposable Db2 LUW test database and run:

```sh
cargo test --test db2 -- --ignored --nocapture
```

It checks Unicode, numeric/date/binary values, NULL, parameter binding,
multi-block fetching, SQL errors, transactions, and cancellation of an actual
server lock wait (confirmed through `MON_GET_APPL_LOCKWAIT` before cancellation).
The cancellation test requires permission to use that monitoring function.
It creates unique tables in the user's schema
and drops it on success; an interrupted or failed run may leave that test table.
The account needs CREATE TABLE, INSERT, SELECT, DELETE, and DROP permissions.
CI runs offline unit tests, documentation examples, and checks on Linux,
Windows, and macOS, plus a Rust 1.89 MSRV job. The real-server test is opt-in.

For a disposable Db2 LUW 12.1.5 instance with TCP and native TLS, see
[tests/docker/README.md](tests/docker/README.md). Set `DB2_TLS_CA` to its exported
PEM certificate and point `DB2_URL` at the TLS port to run the same integration
tests through TLS. Local transport tests generate temporary certificates and
check private CA trust, hostname mismatch, untrusted certificates, and blocked
TCP/TLS cancellation without requiring Db2.
