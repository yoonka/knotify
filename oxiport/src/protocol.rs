//! Opt-in bounded JSON-lines transport; success follows Store::flush.
use anyhow::{Result, anyhow};
use oxigraph::{
    sparql::results::{QueryResultsFormat, QueryResultsSerializer},
    sparql::{Query, QueryResults, Update},
    store::Store,
};
use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};
pub fn run() -> Result<()> {
    let path = std::env::args_os()
        .skip_while(|a| a != "--location")
        .nth(1)
        .ok_or_else(|| anyhow!("missing --location"))?;
    let store = Store::open(path)?;
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
            return Err(anyhow!("packet_limit"));
        }
        let r: Value = serde_json::from_str(&line)?;
        let result = (|| -> Result<Value> {
            if r["version"] != 1 {
                return Err(anyhow!("incompatible_protocol"));
            }
            let p = &r["payload"];
            match r["operation"].as_str().unwrap_or("") {
                "health" => {
                    Ok(json!({"protocol":1,"operations":["query","update"],"durable":true}))
                }
                "update" => {
                    store.update(Update::parse(
                        p["sparql"]
                            .as_str()
                            .ok_or_else(|| anyhow!("missing_sparql"))?,
                        None,
                    )?)?;
                    store.flush()?;
                    Ok(json!({"committed":true}))
                }
                "query" => {
                    let results = store.query(Query::parse(
                        p["sparql"]
                            .as_str()
                            .ok_or_else(|| anyhow!("missing_sparql"))?,
                        None,
                    )?)?;
                    let mut bytes = vec![];
                    match results {
                        QueryResults::Solutions(rows) => {
                            let mut writer =
                                QueryResultsSerializer::from_format(QueryResultsFormat::Json)
                                    .serialize_solutions_to_writer(
                                        &mut bytes,
                                        rows.variables().to_vec(),
                                    )?;
                            for (row_index, row) in rows.enumerate() {
                                if row_index >= 1000 {
                                    return Err(anyhow!("result_limit"));
                                }
                                writer.serialize(&row?)?;
                            }
                            writer.finish()?;
                        }
                        QueryResults::Boolean(b) => {
                            QueryResultsSerializer::from_format(QueryResultsFormat::Json)
                                .serialize_boolean_to_writer(&mut bytes, b)?;
                        }
                        _ => return Err(anyhow!("select_or_ask_required")),
                    }
                    Ok(serde_json::from_slice(&bytes)?)
                }
                _ => Err(anyhow!("unknown_operation")),
            }
        })();
        let reply = match result {
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
