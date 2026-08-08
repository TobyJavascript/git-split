use clap::{Parser, Subcommand};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const DEFAULT_CHUNK_SIZE: u64 = 100 * 1024 * 1024; // 100 MiB
const BUFFER_SIZE: usize = 8 * 1024 * 1024; // 8 MiB read buffer
const MANIFEST_NAME: &str = "manifest.json";
const CONFIG_NAME: &str = ".gitsplit.toml";

#[derive(Serialize, Deserialize, Debug)]
struct Manifest {
    original_filename: String,
    original_size: u64,
    chunk_size: u64,
    chunk_count: usize,
    chunks: Vec<String>,
    original_sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Config {
    #[serde(default = "default_chunk_size")]
    chunk_size: u64,
}

fn default_chunk_size() -> u64 {
    100
}

#[derive(Parser)]
#[command(name = "git-split")]
#[command(about = "Split and assemble large files for GitHub")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan parent folder (recursively) and split files over configured size
    Split,
    /// Scan parent folder (recursively) for .split directories and reassemble
    Assemble,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Split => split_files(),
        Commands::Assemble => assemble_files(),
    }
}

fn load_config(work_dir: &Path) -> u64 {
    let path = work_dir.join(CONFIG_NAME);
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<Config>(&content) {
                Ok(cfg) => cfg.chunk_size * 1024 * 1024,
                Err(_e) => {
                    eprintln!(
                        "Warning: failed to parse '{}', using default ({})",
                        path.display(),
                        default_chunk_size()
                    );
                    DEFAULT_CHUNK_SIZE
                }
            },
            Err(_e) => {
                eprintln!(
                    "Warning: failed to read '{}', using default ({})",
                    path.display(),
                    default_chunk_size()
                );
                DEFAULT_CHUNK_SIZE
            }
        }
    } else {
        DEFAULT_CHUNK_SIZE
    }
}

fn split_files() {
    let cwd = std::env::current_dir().expect("Failed to get working directory");
    let work_dir = cwd.parent().unwrap_or(&cwd).to_path_buf();
    let chunk_size = load_config(&work_dir);

    let mut builder = WalkBuilder::new(&work_dir);
    builder.add_custom_ignore_filename(".splitignore");
    builder.filter_entry(|e| {
        !e.file_name().to_string_lossy().ends_with(".split")
    });

    for result in builder.build() {
        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Warning: {}", e);
                continue;
            }
        };

        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();

        if let Ok(rel) = path.strip_prefix(&work_dir) {
            if let Some(first) = rel.components().next() {
                if first.as_os_str() == "git-split" {
                    continue;
                }
            }
        }

        let size = match entry.metadata() {
            Ok(m) => m.len(),
            Err(_) => continue,
        };

        if size > chunk_size {
            if let Err(e) = split_one(path, size, chunk_size) {
                eprintln!("Failed to split '{}': {}", path.display(), e);
            }
        }
    }
}

fn split_one(path: &Path, size: u64, chunk_size: u64) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"))?;

    println!(
        "Splitting '{}' ({} bytes, chunk size {} MiB)",
        path.display(),
        size,
        chunk_size / 1024 / 1024
    );

    let split_dir = path.with_extension("split");
    if split_dir.exists() {
        fs::remove_dir_all(&split_dir)?;
    }
    fs::create_dir(&split_dir)?;

    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();

    let mut buf = vec![0u8; BUFFER_SIZE];
    let mut chunks = Vec::new();

    loop {
        let idx = chunks.len();
        let chunk_name = format!("chunk_{:03}", idx);
        let chunk_path = split_dir.join(&chunk_name);
        let out = File::create(&chunk_path)?;
        let mut writer = BufWriter::new(out);

        let mut written: u64 = 0;
        while written < chunk_size {
            let want = std::cmp::min(buf.len() as u64, chunk_size - written) as usize;
            let n = reader.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            written += n as u64;
        }

        writer.flush()?;
        drop(writer);

        if written == 0 {
            fs::remove_file(chunk_path)?;
            break;
        }

        chunks.push(chunk_name);
        println!("  -> wrote {}", chunks.last().unwrap());
    }

    let manifest = Manifest {
        original_filename: name.to_string(),
        original_size: size,
        chunk_size,
        chunk_count: chunks.len(),
        chunks,
        original_sha256: format!("{:x}", hasher.finalize()),
    };

    let manifest_path = split_dir.join(MANIFEST_NAME);
    let mfile = File::create(&manifest_path)?;
    let mut mwriter = BufWriter::new(mfile);
    serde_json::to_writer_pretty(&mut mwriter, &manifest)?;
    mwriter.flush()?;

    fs::remove_file(path)?;

    println!(
        "  Done: {} chunks in '{}' (original removed)",
        manifest.chunk_count,
        split_dir.display()
    );

    Ok(())
}

fn assemble_files() {
    let cwd = std::env::current_dir().expect("Failed to get working directory");
    let work_dir = cwd.parent().unwrap_or(&cwd);

    let mut builder = WalkBuilder::new(work_dir);
    builder.git_ignore(false);
    builder.hidden(false);

    let split_dirs: Vec<_> = builder
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map_or(false, |ft| ft.is_dir()))
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                == Some("split")
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    if split_dirs.is_empty() {
        println!("No .split directories found.");
        return;
    }

    for dir in split_dirs {
        if let Err(e) = assemble_one(&dir) {
            eprintln!("Failed to assemble '{}': {}", dir.display(), e);
        }
    }
}

fn assemble_one(split_dir: &Path) -> io::Result<()> {
    let manifest_path = split_dir.join(MANIFEST_NAME);
    let mfile = File::open(&manifest_path)?;
    let manifest: Manifest =
        serde_json::from_reader(mfile).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let out_path = split_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join(&manifest.original_filename);

    println!(
        "Assembling '{}' ({} bytes, {} chunks)",
        manifest.original_filename, manifest.original_size, manifest.chunk_count
    );

    let out = File::create(&out_path)?;
    let mut writer = BufWriter::new(out);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUFFER_SIZE];

    for chunk_name in &manifest.chunks {
        let chunk_path = split_dir.join(chunk_name);
        let chunk = File::open(&chunk_path)?;
        let mut reader = BufReader::new(chunk);

        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
        }

        println!("  -> merged {}", chunk_name);
    }

    writer.flush()?;
    drop(writer);

    let hash = format!("{:x}", hasher.finalize());
    if hash != manifest.original_sha256 {
        eprintln!("  ERROR: SHA-256 mismatch — file may be corrupted!");
        eprintln!("  Expected: {}", manifest.original_sha256);
        eprintln!("  Got:      {}", hash);
        return Err(io::Error::new(io::ErrorKind::InvalidData, "checksum mismatch"));
    }

    println!("  Verified and written to '{}'", out_path.display());
    Ok(())
}
