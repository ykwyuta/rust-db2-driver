//! Run with DB2_URL pointing to a Db2 LUW development database.
//! Creates a unique demo schema and table. Creates Explain tables in the session
//! authorization schema only if absent, and removes only objects it created.
use rust_db2_driver::{Connection, Rows, SqlCa, Value};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn success(ca: SqlCa) -> Result<()> {
    if ca.sqlcode < 0 {
        return Err(format!("SQL error: {ca:?}").into());
    }
    Ok(())
}

fn query(c: &mut Connection, sql: &str) -> Result<Rows> {
    let rows = c.query(sql)?;
    success(rows.sqlca.clone())?;
    Ok(rows)
}

fn main() -> Result<()> {
    let params = rust_db2_driver::Params::from_url(&std::env::var("DB2_URL")?)?;
    let mut c = Connection::connect(&params)?;
    let auth = query(&mut c, "VALUES SESSION_USER")?;
    let Value::Str(auth) = &auth.rows[0][0] else {
        return Err("expected a session authorization ID".into());
    };
    let auth_literal = auth.trim().replace('\'', "''");
    let explain = format!("\"{}\"", auth.trim().replace('"', "\"\""));
    let existing = query(
        &mut c,
        &format!(
            "SELECT COUNT(*) FROM SYSCAT.TABLES WHERE TABSCHEMA = '{auth_literal}' \
         AND TABNAME LIKE 'EXPLAIN_%'"
        ),
    )?;
    let has_explain = existing.rows[0][0] != Value::Int(0);
    let tag = format!(
        "RDB{:016X}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos() as u64
    );
    let keys = "EXPLAIN_REQUESTER, EXPLAIN_TIME, SOURCE_NAME, SOURCE_SCHEMA, SOURCE_VERSION, EXPLAIN_LEVEL, STMTNO, SECTNO";
    let filter = format!(
        "({keys}) IN (SELECT {keys} FROM {explain}.EXPLAIN_STATEMENT WHERE QUERYTAG = '{tag}')"
    );
    let schema = format!(
        "RDBEXP{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    success(c.execute(&format!("CREATE SCHEMA {schema}"))?)?;
    let mut demo_created = false;
    let mut explain_created = false;
    let result = (|| -> Result<()> {
        success(c.execute(&format!(
            "CREATE TABLE {schema}.DEMO (ID INTEGER NOT NULL PRIMARY KEY, NAME VARCHAR(40))"
        ))?)?;
        demo_created = true;
        success(c.execute(&format!(
            "INSERT INTO {schema}.DEMO SELECT INTEGER(ROW_NUMBER() OVER()), \
             CAST('row' AS VARCHAR(40)) FROM SYSCAT.COLUMNS FETCH FIRST 1000 ROWS ONLY"
        ))?)?;
        success(c.commit()?)?;
        if !has_explain {
            success(c.execute(&format!(
                "CALL SYSPROC.SYSINSTALLOBJECTS('EXPLAIN', 'C', \
             CAST(NULL AS VARCHAR(128)), '{auth_literal}')"
            ))?)?;
            explain_created = true;
        }
        success(c.commit()?)?;
        let sql = format!("SELECT NAME FROM {schema}.DEMO WHERE ID = 42");
        println!("SQL: {sql}");
        success(c.execute(&format!("EXPLAIN PLAN SET QUERYTAG = '{tag}' FOR {sql}"))?)?;

        let operators = query(
            &mut c,
            &format!(
                "SELECT OPERATOR_ID, OPERATOR_TYPE, TOTAL_COST FROM {explain}.EXPLAIN_OPERATOR \
             WHERE {filter} \
             ORDER BY OPERATOR_ID"
            ),
        )?;
        if operators.rows.is_empty() {
            return Err("EXPLAIN returned no operators".into());
        }
        println!("Operators (id, type, cumulative estimated cost in timerons):");
        for row in operators.rows {
            println!("{row:?}");
        }
        let streams = query(
            &mut c,
            &format!(
                "SELECT SOURCE_TYPE, SOURCE_ID, TARGET_TYPE, TARGET_ID, STREAM_COUNT, \
             OBJECT_SCHEMA, OBJECT_NAME FROM {explain}.EXPLAIN_STREAM \
             WHERE {filter} \
             ORDER BY STREAM_ID"
            ),
        )?;
        if streams.rows.is_empty() {
            return Err("EXPLAIN returned no streams".into());
        }
        println!("Streams (source type/id, target type/id, estimated rows, object schema/name):");
        for row in streams.rows {
            println!("{row:?}");
        }
        let statements = query(
            &mut c,
            &format!(
                "SELECT EXPLAIN_LEVEL, CAST(STATEMENT_TEXT AS VARCHAR(4000)) \
             FROM {explain}.EXPLAIN_STATEMENT WHERE QUERYTAG = '{tag}'"
            ),
        )?;
        println!("Statements (level, text; CLOB cast to VARCHAR(4000)):");
        for row in statements.rows {
            println!("{row:?}");
        }
        Ok(())
    })();

    // Discard this run's Explain rows without deleting existing plans.
    success(c.rollback()?)?;
    // Attempt every cleanup even when the demonstration failed.
    let mut cleanup_error = None;
    let mut cleanup = |sql: String| {
        let r = c.execute(&sql).map_err(Into::into).and_then(success);
        if let Err(e) = r {
            eprintln!("Cleanup failed for {sql}: {e}");
            cleanup_error = Some(e);
        }
    };
    if explain_created {
        cleanup(format!(
            "CALL SYSPROC.SYSINSTALLOBJECTS('EXPLAIN', 'D', \
             CAST(NULL AS VARCHAR(128)), '{auth_literal}')"
        ));
    }
    if demo_created {
        cleanup(format!("DROP TABLE {schema}.DEMO"));
    }
    cleanup(format!("DROP SCHEMA {schema} RESTRICT"));
    success(c.commit()?)?;
    result?;
    if let Some(e) = cleanup_error {
        return Err(e);
    }
    println!("Created objects removed; existing Explain tables preserved.");
    Ok(())
}
