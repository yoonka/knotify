use clap::Parser;
use duckdb::Connection;
use serde_json::{Value, json};
use std::error::Error;
use std::io::{self, BufRead};
mod protocol;

#[derive(Parser, Debug)]
#[command(name = "duckport")]
#[command(about = "DuckDB query processor", long_about = None)]
struct Args {
    #[arg(short, long)]
    database: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|a| a == "--jsonl") {
        return protocol::run();
    }
    let args = Args::parse();
    let conn = Connection::open(&args.database)?;
    let stdin = io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let query = line.map_err(|e| Box::new(e) as Box<dyn Error>)?;
        let query = query.trim();

        if query.is_empty() {
            continue;
        }

        match execute_query(&conn, query) {
            Ok(result) => println!("{}", result),
            Err(e) => println!("{}", json!({"error": e.to_string()})),
        }
    }

    Ok(())
}

fn execute_query(conn: &Connection, query: &str) -> Result<String, Box<dyn Error>> {
    let mut stmt = conn.prepare(query)?;
    // Execute each statement exactly once. Preparing and querying a second
    // statement would repeat mutations such as INSERT, UPDATE, and DELETE.
    let mut rows = stmt.query([])?;
    let result_stmt = rows
        .as_ref()
        .ok_or("query rows did not retain their prepared statement")?;
    let column_count = result_stmt.column_count();
    let mut col_names = Vec::new();
    for i in 0..column_count {
        col_names.push(result_stmt.column_name(i)?.to_string());
    }

    let mut rows_data = Vec::new();

    while let Some(row) = rows.next()? {
        let mut row_obj = serde_json::Map::new();

        for i in 0..col_names.len() {
            let col_name = &col_names[i];
            let value: Value = match row.get_ref(i)? {
                duckdb::types::ValueRef::Null => Value::Null,
                duckdb::types::ValueRef::TinyInt(v) => json!(v),
                duckdb::types::ValueRef::SmallInt(v) => json!(v),
                duckdb::types::ValueRef::Int(v) => json!(v),
                duckdb::types::ValueRef::BigInt(v) => json!(v),
                duckdb::types::ValueRef::Float(v) => json!(v),
                duckdb::types::ValueRef::Double(v) => json!(v),
                duckdb::types::ValueRef::Text(v) => json!(String::from_utf8_lossy(v).to_string()),
                duckdb::types::ValueRef::Blob(v) => {
                    json!(format!("[binary data: {} bytes]", v.len()))
                }
                _ => Value::Null,
            };
            row_obj.insert(col_name.to_string(), value);
        }

        rows_data.push(Value::Object(row_obj));
    }

    Ok(json!({"rows": rows_data}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executes_crud_statements_once() -> Result<(), Box<dyn Error>> {
        let conn = Connection::open_in_memory()?;

        execute_query(
            &conn,
            "CREATE TABLE items (id INTEGER PRIMARY KEY, value VARCHAR)",
        )?;
        execute_query(&conn, "INSERT INTO items VALUES (1, 'created')")?;

        let created: Value = serde_json::from_str(&execute_query(
            &conn,
            "SELECT value FROM items WHERE id = 1",
        )?)?;
        assert_eq!(created["rows"][0]["value"], json!("created"));

        execute_query(&conn, "UPDATE items SET value = 'updated' WHERE id = 1")?;

        let updated: Value = serde_json::from_str(&execute_query(
            &conn,
            "SELECT value FROM items WHERE id = 1",
        )?)?;
        assert_eq!(updated["rows"][0]["value"], json!("updated"));

        execute_query(&conn, "DELETE FROM items WHERE id = 1")?;

        let deleted: Value = serde_json::from_str(&execute_query(
            &conn,
            "SELECT COUNT(*) AS remaining FROM items WHERE id = 1",
        )?)?;
        assert_eq!(deleted["rows"][0]["remaining"], json!(0));

        execute_query(&conn, "DROP TABLE items")?;

        Ok(())
    }
}
