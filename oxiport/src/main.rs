use anyhow::{Context, Result, bail};
use clap::Parser;
use oxigraph::io::{RdfFormat, RdfSerializer};
use oxigraph::sparql::results::{QueryResultsFormat, QueryResultsSerializer};
use oxigraph::sparql::{
    BoundPreparedSparqlQuery, QueryEvaluationError, QueryExplanation, QueryResults, SparqlEvaluator,
};
use oxigraph::store::Store;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufWriter, Write, stdin, stdout};
use std::path::Path;
mod protocol;

#[cfg(feature = "geosparql")]
use spargeo::GEOSPARQL_EXTENSION_FUNCTIONS;

#[derive(Parser, Debug)]
#[clap(
    name = "oxiport",
    version = "0.1.0",
    about = "Interactive CLI for Oxigraph"
)]
struct Cli {
    #[clap(short, long, value_hint = clap::ValueHint::DirPath)]
    location: Option<String>,
    #[clap(long)]
    query_base: Option<String>,
    #[clap(long)]
    results_format: Option<String>,
    #[clap(long)]
    results_file: Option<std::path::PathBuf>,
    #[clap(long)]
    union_default_graph: bool,
    #[clap(long)]
    update_base: Option<String>,
    #[clap(short, long)]
    quiet: bool,
}

fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--jsonl") {
        return protocol::run();
    }

    let Cli {
        location,
        query_base,
        results_format,
        results_file,
        union_default_graph,
        update_base,
        quiet,
    } = Cli::parse();

    let store = if let Some(location) = location {
        Store::open(location)?
    } else {
        Store::new()?
    };

    let stdin = stdin();
    let mut stdout = stdout();
    let mut input = String::new();

    if !quiet {
        eprintln!("Oxigraph interactive mode. Type 'query', 'update', 'clear', or 'exit'.");
    }

    loop {
        input.clear();
        if !quiet {
            write!(stdout, "oxigraph> ")?;
            stdout.flush()?;
        }
        if stdin.lock().read_line(&mut input)? == 0 {
            break;
        }

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();

        match command.to_ascii_lowercase().as_str() {
            "query" => {
                if let Err(e) = handle_query(
                    Some(&store),
                    None,
                    Some(rest.to_string()),
                    None,
                    query_base.as_deref(),
                    results_file.as_ref(),
                    results_format.as_deref(),
                    false,
                    None,
                    false,
                    union_default_graph,
                ) {
                    eprintln!("Error: {:?}", e);
                }
            }
            "update" => {
                if let Err(e) = handle_update(
                    Some(&store),
                    None,
                    Some(rest.to_string()),
                    None,
                    update_base.as_deref(),
                ) {
                    eprintln!("Error: {:?}", e);
                } else {
                    println!("Update successful");
                }
            }
            "clear" => {
                if !quiet {
                    print!("\x1B[2J\x1B[1;1H");
                    stdout.flush()?;
                }
            }
            "exit" | "quit" => break,
            _ => {
                if !quiet {
                    eprintln!("Unknown command. Use 'query', 'update', 'clear', or 'exit'.");
                }
            }
        }
    }

    Ok(())
}

fn handle_query(
    store: Option<&Store>,
    location: Option<std::path::PathBuf>,
    query: Option<String>,
    query_file: Option<std::path::PathBuf>,
    query_base: Option<&str>,
    results_file: Option<&std::path::PathBuf>,
    results_format: Option<&str>,
    explain: bool,
    explain_file: Option<std::path::PathBuf>,
    stats: bool,
    union_default_graph: bool,
) -> anyhow::Result<()> {
    if store.is_none() && location.is_none() {
        bail!("Either store or location must be provided");
    }
    if store.is_some() && location.is_some() {
        bail!("Only one of store or location can be provided");
    }
    let query = if let Some(query) = query {
        query
    } else if let Some(query_file) = query_file {
        fs::read_to_string(&query_file)
            .with_context(|| format!("Not able to read query file {}", query_file.display()))?
    } else {
        io::read_to_string(stdin().lock())?
    };
    let mut query = sparql_evaluator(query_base)?.parse_query(&query)?;
    if union_default_graph {
        query.dataset_mut().set_default_graph_as_union();
    }
    // Execute the query using either the provided store or by opening a new one
    let (results, explanation) = if let Some(store) = store {
        run_query(query.on_store(store), stats)
    } else {
        // We already validated that location is Some if store is None
        let store = Store::open_read_only(location.unwrap())?;
        run_query(query.on_store(&store), stats)
    };
    let print_result = (|| {
        match results? {
            QueryResults::Solutions(solutions) => {
                let format = if let Some(name) = results_format {
                    if let Some(format) = QueryResultsFormat::from_extension(&name) {
                        format
                    } else if let Some(format) = QueryResultsFormat::from_media_type(&name) {
                        format
                    } else {
                        bail!("The file format '{name}' is unknown")
                    }
                } else if let Some(results_file) = &results_file {
                    format_from_path(results_file, |ext| {
                        QueryResultsFormat::from_extension(ext)
                            .with_context(|| format!("The file extension '{ext}' is unknown"))
                    })?
                } else {
                    // Default to TSV for interactive output
                    QueryResultsFormat::Tsv
                };
                if let Some(results_file) = results_file {
                    let mut serializer = QueryResultsSerializer::from_format(format)
                        .serialize_solutions_to_writer(
                            BufWriter::new(File::create(results_file)?),
                            solutions.variables().to_vec(),
                        )?;
                    for solution in solutions {
                        serializer.serialize(&solution?)?;
                    }
                    close_file_writer(serializer.finish()?)?;
                } else {
                    let mut serializer = QueryResultsSerializer::from_format(format)
                        .serialize_solutions_to_writer(
                            stdout().lock(),
                            solutions.variables().to_vec(),
                        )?;
                    for solution in solutions {
                        serializer.serialize(&solution?)?;
                    }
                    serializer.finish()?.flush()?;
                }
            }
            QueryResults::Boolean(result) => {
                let format = if let Some(name) = results_format {
                    if let Some(format) = QueryResultsFormat::from_extension(&name) {
                        format
                    } else if let Some(format) = QueryResultsFormat::from_media_type(&name) {
                        format
                    } else {
                        bail!("The file format '{name}' is unknown")
                    }
                } else if let Some(results_file) = &results_file {
                    format_from_path(results_file, |ext| {
                        QueryResultsFormat::from_extension(ext)
                            .with_context(|| format!("The file extension '{ext}' is unknown"))
                    })?
                } else {
                    // Default to TSV for interactive output
                    QueryResultsFormat::Tsv
                };
                if let Some(results_file) = results_file {
                    close_file_writer(
                        QueryResultsSerializer::from_format(format).serialize_boolean_to_writer(
                            BufWriter::new(File::create(results_file)?),
                            result,
                        )?,
                    )?;
                } else {
                    QueryResultsSerializer::from_format(format)
                        .serialize_boolean_to_writer(stdout().lock(), result)?
                        .flush()?;
                }
            }
            QueryResults::Graph(triples) => {
                let format = if let Some(name) = &results_format {
                    rdf_format_from_name(name)
                } else if let Some(results_file) = &results_file {
                    rdf_format_from_path(results_file)
                } else {
                    // Default to N-Triples for interactive output
                    Ok(RdfFormat::NTriples)
                }?;
                let serializer = RdfSerializer::from_format(format);
                if let Some(results_file) = results_file {
                    let mut serializer =
                        serializer.for_writer(BufWriter::new(File::create(results_file)?));
                    for triple in triples {
                        serializer.serialize_triple(triple?.as_ref())?;
                    }
                    close_file_writer(serializer.finish()?)?;
                } else {
                    let mut serializer = serializer.for_writer(stdout().lock());
                    for triple in triples {
                        serializer.serialize_triple(triple?.as_ref())?;
                    }
                    serializer.finish()?.flush()?;
                }
            }
        }
        Ok(())
    })();
    if let Some(explain_file) = explain_file {
        let mut file = BufWriter::new(File::create(&explain_file)?);
        match explain_file.extension().and_then(OsStr::to_str) {
            Some("json") => {
                explanation.write_in_json(&mut file)?;
            }
            Some("txt") => {
                write!(file, "{explanation:?}")?;
            }
            _ => bail!(
                "The given explanation file {} must have an extension that is .json or .txt",
                explain_file.display()
            ),
        }
        close_file_writer(file)?;
    } else if explain || stats {
        eprintln!("{explanation:#?}");
    }
    print_result
}

fn handle_update(
    store: Option<&Store>,
    location: Option<std::path::PathBuf>,
    update: Option<String>,
    update_file: Option<std::path::PathBuf>,
    update_base: Option<&str>,
) -> anyhow::Result<()> {
    if store.is_none() && location.is_none() {
        bail!("Either store or location must be provided");
    }
    if store.is_some() && location.is_some() {
        bail!("Only one of store or location can be provided");
    }
    let update = if let Some(update) = update {
        update
    } else if let Some(update_file) = update_file {
        fs::read_to_string(&update_file)
            .with_context(|| format!("Not able to read update file {}", update_file.display()))?
    } else {
        io::read_to_string(stdin().lock())?
    };
    let update = sparql_evaluator(update_base)?.parse_update(&update)?;
    if let Some(store) = store {
        update.on_store(store).execute()?;
        store.flush()?;
    } else {
        let store = Store::open(location.unwrap())?;
        update.on_store(&store).execute()?;
        store.flush()?;
    }

    Ok(())
}

/// The evaluator for queries and updates: the base IRI, if given, and the
/// GeoSPARQL functions with the geosparql feature.
fn sparql_evaluator(base_iri: Option<&str>) -> anyhow::Result<SparqlEvaluator> {
    let mut evaluator = SparqlEvaluator::new();
    if let Some(base_iri) = base_iri {
        evaluator = evaluator.with_base_iri(base_iri)?;
    }
    #[cfg(feature = "geosparql")]
    for (name, function) in GEOSPARQL_EXTENSION_FUNCTIONS {
        evaluator = evaluator.with_custom_function(name.into_owned(), function);
    }
    Ok(evaluator)
}

/// Runs a query and returns its results with the explanation of the plan,
/// including execution statistics if asked for.
fn run_query(
    query: BoundPreparedSparqlQuery<'static>,
    stats: bool,
) -> (
    Result<QueryResults<'static>, QueryEvaluationError>,
    QueryExplanation,
) {
    if stats {
        query.compute_statistics().explain()
    } else {
        query.explain()
    }
}

fn format_from_path<T>(
    path: &Path,
    from_extension: impl FnOnce(&str) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if let Some(ext) = path.extension().and_then(OsStr::to_str) {
        from_extension(ext).map_err(|e| {
            e.context(format!(
                "Not able to guess the file format from file name extension '{ext}'"
            ))
        })
    } else {
        bail!(
            "The path {} has no extension to guess a file format from",
            path.display()
        )
    }
}

fn rdf_format_from_path(path: &Path) -> anyhow::Result<RdfFormat> {
    format_from_path(path, |ext| {
        RdfFormat::from_extension(ext)
            .with_context(|| format!("The file extension '{ext}' is unknown"))
    })
}

fn rdf_format_from_name(name: &str) -> anyhow::Result<RdfFormat> {
    if let Some(t) = RdfFormat::from_extension(name) {
        return Ok(t);
    }
    if let Some(t) = RdfFormat::from_media_type(name) {
        return Ok(t);
    }
    bail!("The file format '{name}' is unknown")
}

fn close_file_writer(writer: BufWriter<File>) -> io::Result<()> {
    let mut file = writer
        .into_inner()
        .map_err(io::IntoInnerError::into_error)?;
    file.flush()?;
    file.sync_all()
}
