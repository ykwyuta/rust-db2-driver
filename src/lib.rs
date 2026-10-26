//! A synchronous Db2 driver implementing DRDA directly over TCP.
//!
//! No ODBC, IBM client libraries, or external Rust dependencies are required.
//!
//! ```no_run
//! use rust_db2_driver::{Connection, Params, Value};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let params = Params::from_url(&std::env::var("DB2_URL")?)?;
//! let mut conn = Connection::connect(&params)?;
//! let result = conn.query("VALUES (42, CAST('hello' AS VARCHAR(20)))")?;
//! if result.sqlca.sqlcode < 0 {
//!     return Err(format!("SQL error: {:?}", result.sqlca).into());
//! }
//! assert_eq!(result.rows[0][0], Value::Int(42));
//! conn.rollback()?;
//! # Ok(())
//! # }
//! ```
//!
//! Transport and protocol failures return [`Error`]. SQL failures are returned
//! in [`SqlCa`]: check `sqlcode < 0` even when a method returns `Ok`.
//! Transactions require explicit [`Connection::commit`] or
//! [`Connection::rollback`]. Queries buffer their complete result in memory.
//!
//! This initial implementation targets Db2 LUW and uses unencrypted TCP with
//! DRDA user/password authentication (SECMEC 3). TLS, parameter binding,
//! asynchronous I/O, and streaming cursors are not implemented.

pub mod client;
pub mod ddm;
mod ebcdic;
pub mod sql;

pub use client::{Connection, Error, Params, Result};
pub use sql::{Column, Rows, SqlCa, Value};
