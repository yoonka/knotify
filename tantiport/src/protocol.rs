//! Opt-in protocol v1. The legacy demonstration/search mode is unchanged.
use serde_json::{Value, json};
use std::{
    error::Error,
    io::{self, BufRead, Read, Write},
};
use tantivy::schema::Value as TantivyValue;
use tantivy::{
    Index, Term,
    collector::TopDocs,
    doc,
    query::{AllQuery, BooleanQuery, Occur, Query, QueryParser, TermQuery},
    schema::*,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub fn run() -> Result<()> {
    let path = std::env::args_os()
        .skip_while(|a| a != "--index")
        .nth(1)
        .ok_or("missing --index")?;
    std::fs::create_dir_all(&path)?;
    let mut schema = Schema::builder();
    let id = schema.add_text_field("id", STRING | STORED);
    let hierarchy = schema.add_text_field("hierarchy", STRING | STORED);
    let title = schema.add_text_field("title", TEXT | STORED);
    let body = schema.add_text_field("body", TEXT);
    let payload = schema.add_text_field("payload", STORED);
    let schema = schema.build();
    let index = Index::open_or_create(tantivy::directory::MmapDirectory::open(&path)?, schema)?;
    let mut writer = index.writer_with_num_threads(1, 50_000_000)?;
    let reader = index.reader()?;
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
        let result = (|| -> Result<Value> {
            if r["version"] != 1 {
                return Err("incompatible_protocol".into());
            }
            let p = &r["payload"];
            match r["operation"].as_str().ok_or("missing_operation")? {
                "health" => Ok(
                    json!({"protocol":1,"operations":["upsert","delete","search","list","get"],"durable":true}),
                ),
                "upsert" => {
                    let key = p["id"].as_str().ok_or("missing_id")?;
                    writer.delete_term(Term::from_field_text(id, key));
                    writer.add_document(doc!(id=>key,hierarchy=>p["hierarchy"].as_str().ok_or("missing_hierarchy")?,title=>p["title"].as_str().unwrap_or(""),body=>p["text"].as_str().unwrap_or(""),payload=>p["metadata"].to_string()))?;
                    writer.commit()?;
                    reader.reload()?;
                    Ok(json!({"committed":true}))
                }
                "delete" => {
                    writer.delete_term(Term::from_field_text(
                        id,
                        p["id"].as_str().ok_or("missing_id")?,
                    ));
                    writer.commit()?;
                    reader.reload()?;
                    Ok(json!({"committed":true}))
                }
                op @ ("search" | "list" | "get") => {
                    let limit = p["limit"].as_u64().unwrap_or(100).clamp(1, 100) as usize;
                    let offset = p["offset"].as_u64().unwrap_or(0).min(1_000_000_000) as usize;
                    let base: Box<dyn Query> = if op == "search" {
                        let text = p["query"].as_str().ok_or("missing_query")?;
                        if text.len() > 1024 {
                            return Err("query_limit".into());
                        }
                        Box::new(
                            QueryParser::for_index(&index, vec![title, body])
                                .parse_query(&serde_json::to_string(text)?)?,
                        )
                    } else {
                        Box::new(AllQuery)
                    };
                    let mut clauses = vec![(Occur::Must, base)];
                    if op == "get" {
                        clauses.push((
                            Occur::Must,
                            Box::new(TermQuery::new(
                                Term::from_field_text(id, p["id"].as_str().ok_or("missing_id")?),
                                IndexRecordOption::Basic,
                            )),
                        ));
                    }
                    if let Some(h) = p["hierarchy"].as_str() {
                        clauses.push((
                            Occur::Must,
                            Box::new(TermQuery::new(
                                Term::from_field_text(hierarchy, h),
                                IndexRecordOption::Basic,
                            )),
                        ));
                    }
                    let searcher = reader.searcher();
                    let mut rows = vec![];
                    for (_, address) in searcher.search(
                        &BooleanQuery::new(clauses),
                        &TopDocs::with_limit(limit)
                            .and_offset(offset)
                            .order_by_score(),
                    )? {
                        let d = searcher.doc::<TantivyDocument>(address)?;
                        let mut m: Value = serde_json::from_str(
                            d.get_first(payload)
                                .and_then(|v| v.as_str())
                                .ok_or("missing_payload")?,
                        )?;
                        m["id"] = d
                            .get_first(id)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into();
                        rows.push(m);
                    }
                    Ok(json!({"results":rows}))
                }
                _ => Err("unknown_operation".into()),
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
