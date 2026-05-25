//! T-224 — Golden output generator.
//!
//! Walks a source directory, extracts all symbols, and writes
//! `nodes-full.json` + `edges-full.json` to an output directory.
//!
//! Usage:
//!   generate-golden --input <dir> --output <dir>

use clap::Parser;
use codewiki_core::{Edge, Node};
use codewiki_extraction::ast_walker::extract_file;
use codewiki_extraction::file_reader::read_source_file;
use codewiki_extraction::language_detector::is_source_file;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "generate-golden", about = "Generate golden extraction outputs")]
struct Args {
    /// Source directory to walk
    #[arg(short, long)]
    input: PathBuf,

    /// Output directory for JSON files
    #[arg(short, long)]
    output: PathBuf,
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(path = %dir.display(), err = %e, "read_dir failed; skipping");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn main() {
    // Simple stderr tracing for the binary.
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "warn".to_string())
                .as_str(),
        )
        .init();

    let args = Args::parse();

    // Create output directory if needed.
    if let Err(e) = std::fs::create_dir_all(&args.output) {
        eprintln!("Failed to create output dir: {e}");
        std::process::exit(1);
    }

    let mut all_nodes: Vec<Node> = Vec::new();
    let mut all_edges: Vec<Edge> = Vec::new();

    // Walk files recursively.
    let mut paths: Vec<PathBuf> = Vec::new();
    walk_dir(&args.input, &mut paths);

    let mut file_count = 0usize;

    for path in &paths {
        if !is_source_file(path) {
            continue;
        }
        let source = match read_source_file(path) {
            Some(s) => s,
            None => continue,
        };

        let batch = extract_file(path, &source);
        all_nodes.extend(batch.nodes);
        all_edges.extend(batch.edges);
        file_count += 1;
    }

    eprintln!(
        "Processed {file_count} files; {} nodes, {} edges",
        all_nodes.len(),
        all_edges.len()
    );

    // Write nodes-full.json
    let nodes_path = args.output.join("nodes-full.json");
    match serde_json::to_string_pretty(&all_nodes) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&nodes_path, json) {
                eprintln!("Failed to write nodes: {e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Failed to serialize nodes: {e}");
            std::process::exit(1);
        }
    }

    // Write edges-full.json
    let edges_path = args.output.join("edges-full.json");
    match serde_json::to_string_pretty(&all_edges) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&edges_path, json) {
                eprintln!("Failed to write edges: {e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Failed to serialize edges: {e}");
            std::process::exit(1);
        }
    }

    println!("Golden outputs written to {}", args.output.display());
}
