//! Correlated parameterized queries and atomic mutation batches.
use duckdb::{
    Connection, params_from_iter,
    types::{Value as Db, ValueRef},
};
use serde_json::{Value, json};
use std::{
    error::Error,
    io::{self, BufRead, Read, Write},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn parameters(v: &Value) -> Result<Vec<Db>> {
    v.as_array()
        .ok_or("invalid_parameters")?
        .iter()
        .map(|v| {
            Ok(match v {
                Value::Null => Db::Null,
                Value::Bool(b) => Db::Boolean(*b),
                Value::String(s) => Db::Text(s.clone()),
                Value::Number(n) if n.is_i64() => Db::BigInt(n.as_i64().unwrap()),
                Value::Number(n) => Db::Double(n.as_f64().ok_or("invalid_number")?),
                _ => return Err("invalid_parameter".into()),
            })
        })
        .collect()
}
fn query(conn: &Connection, p: &Value) -> Result<Value> {
    let mut stmt = conn.prepare(p["sql"].as_str().ok_or("missing_sql")?)?;
    let args = parameters(&p["params"])?;
    let mut rows = stmt.query(params_from_iter(args.iter()))?;
    let names = rows
        .as_ref()
        .ok_or("missing_result_statement")?
        .column_names();
    let mut result = vec![];
    while let Some(row) = rows.next()? {
        if result.len() >= 1000 {
            return Err("result_limit".into());
        }
        let mut obj = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Boolean(b) => json!(b),
                ValueRef::TinyInt(n) => json!(n),
                ValueRef::SmallInt(n) => json!(n),
                ValueRef::Int(n) => json!(n),
                ValueRef::BigInt(n) => json!(n.to_string()),
                ValueRef::Float(n) => json!(n),
                ValueRef::Double(n) => json!(n),
                ValueRef::Text(b) => json!(std::str::from_utf8(b)?),
                _ => return Err("unsupported_result_type".into()),
            };
            obj.insert(name.clone(), value);
        }
        result.push(Value::Object(obj));
    }
    Ok(json!({"rows":result}))
}
pub fn run() -> Result<()> {
    let path = std::env::args_os()
        .skip_while(|a| a != "--database")
        .nth(1)
        .ok_or("missing --database")?;
    let mut conn = Connection::open(path)?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut line = String::new();
        if input
            .by_ref()
            .take(16 * 1024 * 1024 + 1)
            .read_line(&mut line)?
            == 0
        {
            break;
        }
        if line.len() > 16 * 1024 * 1024 {
            return Err("packet_limit".into());
        }
        let r: Value = serde_json::from_str(&line)?;
        let response = (|| -> Result<Value> {
            if r["version"] != 1 {
                return Err("incompatible_protocol".into());
            }
            match r["operation"].as_str().ok_or("missing_operation")? {
                "health" => Ok(json!({"protocol":1,"operations":["query","batch"],"durable":true})),
                "query" => query(&conn, &r["payload"]),
                "batch" => {
                    let statements = r["payload"]["statements"]
                        .as_array()
                        .ok_or("missing_statements")?;
                    let tx = conn.transaction()?;
                    for p in statements {
                        let args = parameters(&p["params"])?;
                        tx.execute(
                            p["sql"].as_str().ok_or("missing_sql")?,
                            params_from_iter(args.iter()),
                        )?;
                    }
                    tx.commit()?;
                    Ok(json!({"committed":true}))
                }
                _ => Err("unknown_operation".into()),
            }
        })();
        let reply = match response {
            Ok(data) => json!({"version":1,"request_id":r["request_id"],"ok":true,"data":data}),
            Err(e) => {
                json!({"version":1,"request_id":r["request_id"],"ok":false,"error":e.to_string()})
            }
        };
        writeln!(output, "{reply}")?;
        output.flush()?;
    }
    Ok(())
}
