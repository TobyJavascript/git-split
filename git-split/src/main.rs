use clap::{Parser, Subcommand};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn default_chunk_size() -> u64 {
    100
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug)]
struct HooksConfig {
    #[serde(default = "default_true")]
    pre_commit: bool,
    #[serde(default = "default_true")]
    post_commit: bool,
    #[serde(default = "default_true")]
    post_checkout: bool,
    #[serde(default = "default_true")]
    post_merge: bool,
}

impl Default for HooksConfig {
    fn default() -> Self {
        HooksConfig {
            pre_commit: true,
            post_commit: true,
            post_checkout: true,
            post_merge: true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Config {
    #[serde(default = "default_chunk_size")]
    chunk_size: u64,
    #[serde(default = "default_true")]
    backup: bool,
    #[serde(default)]
    remove_original: bool,
    #[serde(default = "default_true")]
    use_gitignore: bool,
    #[serde(default)]
    hooks: HooksConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            chunk_size: default_chunk_size(),
            backup: true,
            remove_original: false,
            use_gitignore: true,
            hooks: HooksConfig::default(),
        }
    }
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
    /// Install or uninstall git hooks for automatic splitting and assembling
    Hooks {
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,
        #[arg(long, conflicts_with = "install")]
        uninstall: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Split => split_files(),
        Commands::Assemble => assemble_files(),
        Commands::Hooks { install, uninstall } => manage_hooks(install, uninstall),
    }
}

fn find_git_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let work_dir = cwd.parent().unwrap_or(&cwd);

    let output = Command::new("git")
        .args(["-C", &work_dir.to_string_lossy(), "rev-parse", "--git-dir"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let git_dir_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(work_dir.join(git_dir_str))
}

const HOOK_MARKER: &str = "# <git-split-hook>";

fn hook_content(hook_name: &str) -> String {
    let bin_name = if cfg!(windows) {
        "git-split.exe"
    } else {
        "git-split"
    };

    let body = match hook_name {
        "pre-commit" => {
            format!(
                r#"REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT/git-split" || exit 1
./target/release/{} split || exit 1
git -C "$REPO_ROOT" add -A
# Unstage originals kept on disk so only .split/ is committed
find "$REPO_ROOT" -type d -name "*.split" | while read -r splitdir; do
    manifest="$splitdir/manifest.json"
    if [ -f "$manifest" ]; then
        orig=$(grep '"original_filename"' "$manifest" | head -n1 | cut -d'"' -f4)
        if [ -n "$orig" ]; then
            orig_path="${{splitdir%.split}}/$orig"
            git -C "$REPO_ROOT" rm --cached -f "$orig_path" >/dev/null 2>&1 || true
        fi
    fi
done"#,
                bin_name
            )
        }
        "post-commit" | "post-checkout" | "post-merge" => format!(
            r#"REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT/git-split" || exit 1
./target/release/{} assemble"#,
            bin_name
        ),
        _ => "".to_string(),
    };

    format!(
        r#"#!/bin/sh
{}
{}
"#,
        HOOK_MARKER, body
    )
}

fn install_hooks(git_dir: &Path, config: &HooksConfig) -> io::Result<()> {
    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_filters = [
        ("pre-commit", config.pre_commit),
        ("post-commit", config.post_commit),
        ("post-checkout", config.post_checkout),
        ("post-merge", config.post_merge),
    ];

    for &(name, enabled) in &hook_filters {
        if !enabled {
            let path = hooks_dir.join(name);
            if path.exists() {
                let content = fs::read_to_string(&path)?;
                if content.contains(HOOK_MARKER) {
                    println!("Removing disabled hook: {}", name);
                    fs::remove_file(&path)?;
                }
            }
            continue;
        }
        let path = hooks_dir.join(name);
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            if content.contains(HOOK_MARKER) {
                println!("Replacing existing git-split hook: {}", name);
            } else {
                println!(
                    "Skipping '{}': a user-defined hook already exists.",
                    name
                );
                continue;
            }
        } else {
            println!("Installing hook: {}", name);
        }

        let mut file = File::create(&path)?;
        file.write_all(hook_content(name).as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
    }

    println!("Hooks installed successfully.");
    Ok(())
}

fn uninstall_hooks(git_dir: &Path) -> io::Result<()> {
    let hooks_dir = git_dir.join("hooks");
    let hooks = ["pre-commit", "post-commit", "post-checkout", "post-merge"];

    for name in hooks {
        let path = hooks_dir.join(name);
        if !path.exists() {
            continue;
        }

        let content = fs::read_to_string(&path)?;
        if content.contains(HOOK_MARKER) {
            println!("Removing hook: {}", name);
            fs::remove_file(&path)?;
        } else {
            println!(
                "Leaving '{}': not managed by git-split.",
                name
            );
        }
    }

    println!("Hooks uninstalled successfully.");
    Ok(())
}

fn manage_hooks(install: bool, uninstall: bool) {
    let cwd = std::env::current_dir().expect("Failed to get working directory");
    let work_dir = cwd.parent().unwrap_or(&cwd);

    let git_dir = match find_git_dir() {
        Some(d) => d,
        None => {
            eprintln!("Error: not inside a git repository.");
            return;
        }
    };

    if install {
        let config = load_config(work_dir);
        if let Err(e) = install_hooks(&git_dir, &config.hooks) {
            eprintln!("Failed to install hooks: {}", e);
        }
    } else if uninstall {
        if let Err(e) = uninstall_hooks(&git_dir) {
            eprintln!("Failed to uninstall hooks: {}", e);
        }
    } else {
        println!("Usage:");
        println!("  git-split hooks --install    # install auto-split/assemble hooks");
        println!("  git-split hooks --uninstall  # remove managed git-split hooks");
    }
}

fn load_config(work_dir: &Path) -> Config {
    let path = work_dir.join(CONFIG_NAME);
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<Config>(&content) {
                Ok(cfg) => cfg,
                Err(_e) => {
                    eprintln!(
                        "Warning: failed to parse '{}', using default ({})",
                        path.display(),
                        default_chunk_size()
                    );
                    Config::default()
                }
            },
            Err(_e) => {
                eprintln!(
                    "Warning: failed to read '{}', using default ({})",
                    path.display(),
                    default_chunk_size()
                );
                Config::default()
            }
        }
    } else {
        Config::default()
    }
}

fn backup_file(src: &Path, subfolder: &str) -> io::Result<PathBuf> {
    let cwd = std::env::current_dir().expect("Failed to get working directory");
    let backup_dir = cwd.join(".backup").join(subfolder);
    fs::create_dir_all(&backup_dir)?;
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"))?;
    let dest = backup_dir.join(name);
    fs::copy(src, &dest)?;
    Ok(dest)
}

fn split_files() {
    let cwd = std::env::current_dir().expect("Failed to get working directory");
    let work_dir = cwd.parent().unwrap_or(&cwd).to_path_buf();
    let config = load_config(&work_dir);
    let chunk_size = config.chunk_size * 1024 * 1024;

    let mut builder = WalkBuilder::new(&work_dir);
    builder.add_custom_ignore_filename(".splitignore");
    builder.filter_entry(|e| {
        !e.file_name().to_string_lossy().ends_with(".split")
    });
    builder.git_ignore(config.use_gitignore);

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
            if config.backup {
                match backup_file(path, "split") {
                    Ok(dest) => println!("  -> backed up to '{}'", dest.display()),
                    Err(e) => eprintln!("Warning: failed to backup '{}': {}", path.display(), e),
                }
            }
            if let Err(e) = split_one(path, size, chunk_size, config.remove_original) {
                eprintln!("Failed to split '{}': {}", path.display(), e);
            }
        }
    }
}

fn split_one(path: &Path, size: u64, chunk_size: u64, remove_original: bool) -> io::Result<()> {
    if chunk_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chunk size is 0 — refusing to split",
        ));
    }
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

    if remove_original {
        fs::remove_file(path)?;
        println!(
            "  Done: {} chunks in '{}' (original removed)",
            manifest.chunk_count,
            split_dir.display()
        );
    } else {
        println!(
            "  Done: {} chunks in '{}' (original kept)",
            manifest.chunk_count,
            split_dir.display()
        );
    }

    Ok(())
}

fn assemble_files() {
    let cwd = std::env::current_dir().expect("Failed to get working directory");
    let work_dir = cwd.parent().unwrap_or(&cwd);
    let config = load_config(work_dir);

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
        if let Err(e) = assemble_one(&dir, config.backup) {
            eprintln!("Failed to assemble '{}': {}", dir.display(), e);
        }
    }
}

fn assemble_one(split_dir: &Path, backup: bool) -> io::Result<()> {
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

    if backup && out_path.exists() {
        match backup_file(&out_path, "assemble") {
            Ok(dest) => println!("  -> backed up existing to '{}'", dest.display()),
            Err(e) => eprintln!("  Warning: failed to backup existing file: {}", e),
        }
    }

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
