//! DB2_URL=db2://user:password@host:50000/DATABASE cargo run --example query
use rust_db2_driver::{Connection, Params};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let params = Params::from_url(&std::env::var("DB2_URL")?)?;
    let sql = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "VALUES (42, CAST('hello' AS VARCHAR(20)))".into());
    let mut conn = Connection::connect(&params)?;
    let result = conn.query(&sql)?;
    if result.sqlca.sqlcode < 0 {
        return Err(format!("SQL error: {:?}", result.sqlca).into());
    }
    for row in result.rows {
        println!("{row:?}");
    }
    let ca = conn.rollback()?;
    if ca.sqlcode < 0 {
        return Err(format!("Rollback failed: {ca:?}").into());
    }
    Ok(())
}
