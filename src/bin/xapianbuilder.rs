// SPDX-License-Identifier: GPL-3.0-or-later
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::{
    error::Error,
    fs::File,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
};
use zimfile_xapian::IndexWriter;

#[derive(Parser)]
#[command(
    version,
    about = "Build a single-file Glass search index from JSON Lines"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a full-text index from article bodies and titles.
    Fulltext(BuildOptions),
    /// Build a title suggestion index, including redirects.
    Title(BuildOptions),
    /// Print the helper version.
    Version,
}

#[derive(clap::Args)]
struct BuildOptions {
    /// Input JSON Lines file. Use - for standard input.
    #[arg(long)]
    input: PathBuf,
    /// Destination for the finished index. Must not exist.
    #[arg(long)]
    output: PathBuf,
    /// Default document language, e.g. eng.
    #[arg(long, default_value = "")]
    language: String,
    /// Omit the file when no documents were indexed.
    #[arg(long)]
    skip_if_empty: bool,
    /// Select single-threaded building (also the default).
    #[arg(long = "jobs", value_parser = ["1"], value_name = "1")]
    _jobs: Option<String>,
    /// Suppress the completion message.
    #[arg(long)]
    quiet: bool,
}

#[derive(Deserialize)]
struct Record {
    path: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    mimetype: String,
    language: Option<String>,
    target_path: Option<String>,
}

fn build(args: BuildOptions, fulltext: bool) -> Result<(), Box<dyn Error>> {
    let parent = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if args.output.try_exists()? {
        return Err(format!("output already exists: {}", args.output.display()).into());
    }

    let reader: Box<dyn BufRead> = if args.input == Path::new("-") {
        Box::new(io::stdin().lock())
    } else {
        Box::new(BufReader::new(
            File::open(&args.input)
                .map_err(|e| format!("opening {}: {e}", args.input.display()))?,
        ))
    };
    let mut writer = if fulltext {
        IndexWriter::fulltext(&args.language)
    } else {
        IndexWriter::title(&args.language)
    };
    let mut active_language = args.language.clone();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let number = index + 1;
        let record: Record =
            serde_json::from_str(&line).map_err(|e| format!("input line {number}: {e}"))?;
        let language = record
            .language
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or(&args.language);
        let target = record.target_path.as_deref().filter(|v| !v.is_empty());
        if record.path.contains('\0')
            || record.title.contains('\0')
            || language.contains('\0')
            || (!fulltext && target.is_some_and(|v| v.contains('\0')))
        {
            eprintln!("xapianbuilder: skipping line {number} with a NUL in an entry field");
            continue;
        }
        if language != active_language {
            writer.set_stemming_language(language);
            active_language.clear();
            active_language.push_str(language);
        }
        if !fulltext {
            writer
                .add_title(&record.path, &record.title, target)
                .map_err(|e| format!("input line {number}: {e}"))?;
        } else if record.mimetype.starts_with("text/html") {
            writer.add_html(&record.path, &record.title, &record.body);
        } else {
            writer.add_text(&record.path, &record.title, &record.body);
        }
    }
    if writer.is_empty() && args.skip_if_empty {
        return Ok(());
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    writer.write(file.as_file_mut(), *uuid::Uuid::new_v4().as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&args.output)
        .map_err(|e| format!("publishing {}: {}", args.output.display(), e.error))?;
    if !args.quiet {
        eprintln!("Wrote {}", args.output.display());
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let result = match Cli::parse().command {
        Command::Fulltext(args) => build(args, true),
        Command::Title(args) => build(args, false),
        Command::Version => {
            println!("xapianbuilder {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xapianbuilder: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
