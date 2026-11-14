use clap::Parser;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, IndexWriter, doc};
use tempfile::TempDir;
mod protocol;

#[derive(Parser, Debug)]
#[command(name = "tantiport")]
#[command(about = "Tantivy full-text search port", long_about = None)]
struct Args {
    /// Directory containing the persistent Tantivy index.
    ///
    /// When omitted, a temporary index is used for standalone development.
    #[arg(long, value_name = "PATH")]
    index: Option<PathBuf>,
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
}

#[derive(Serialize)]
struct SearchResponse {
    results: Vec<serde_json::Value>,
}

fn main() -> tantivy::Result<()> {
    if std::env::args().any(|a| a == "--jsonl") {
        if let Err(e) = protocol::run() {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let args = Args::parse();
    let temp_dir = args.index.is_none().then(TempDir::new).transpose()?;
    let index_path = args
        .index
        .as_deref()
        .or_else(|| temp_dir.as_ref().map(TempDir::path))
        .expect("an explicit or temporary index path must be available");

    fs::create_dir_all(index_path)?;

    // First, we need to define a schema for our search index
    let mut schema_builder = Schema::builder();

    // Define fields
    let id = schema_builder.add_text_field("id", STORED);
    let title = schema_builder.add_text_field("title", TEXT | STORED);
    let body = schema_builder.add_text_field("body", TEXT);
    let schema = schema_builder.build();

    // Open an existing index or create it on first launch. MmapDirectory is
    // supported on Windows and keeps the on-disk index reusable across port
    // process restarts.
    let directory = MmapDirectory::open(index_path)?;
    let index = Index::open_or_create(directory, schema.clone())?;

    let reader = index.reader()?;

    // Seed the demonstration documents only for a newly-created/empty index.
    // Existing persistent indexes must not receive duplicates on every restart.
    if reader.searcher().num_docs() == 0 {
        let mut index_writer: IndexWriter = index.writer(50_000_000)?;

        index_writer.add_document(doc!(
            id => "1",
            title => "The Old Man and the Sea",
            body => "He was an old man who fished alone in a skiff in the Gulf Stream and he had gone eighty-four days now without taking a fish."
        ))?;

        index_writer.add_document(doc!(
            id => "2",
            title => "Of Mice and Men",
            body => "A few miles south of Soledad, the Salinas River drops in close to the hillside bank and runs deep and green."
        ))?;

        index_writer.add_document(doc!(
            id => "3",
            title => "The Great Gatsby",
            body => "In my younger and more vulnerable years my father gave me some advice that I've carried with me ever since."
        ))?;

        index_writer.commit()?;
        reader.reload()?;
    }

    // Create a reader and searcher (keep these for the entire session)
    let query_parser = QueryParser::for_index(&index, vec![title, body]);

    // Interactive mode: read queries from stdin
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };

        // Parse JSON request
        let request: SearchRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(_) => continue,
        };

        // Perform search
        let mut results = Vec::new();

        if let Ok(query) = query_parser.parse_query(&request.query) {
            let searcher = reader.searcher();
            if let Ok(top_docs) = searcher.search(&query, &TopDocs::with_limit(10).order_by_score())
            {
                for (_score, doc_address) in top_docs {
                    if let Ok(retrieved_doc) = searcher.doc::<TantivyDocument>(doc_address) {
                        if retrieved_doc.get_first(title).is_some() {
                            if let Ok(json_doc) =
                                serde_json::from_str(&retrieved_doc.to_json(&schema))
                            {
                                results.push(json_doc);
                            }
                        }
                    }
                }
            }
        }

        // Send JSON response
        let response = SearchResponse { results };
        if let Ok(json_response) = serde_json::to_string(&response) {
            println!("{}", json_response);
            stdout.flush().unwrap();
        }
    }

    Ok(())
}
