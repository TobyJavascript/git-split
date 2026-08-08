# git-split

Split large files (>100 MiB) into smaller chunks for hosting on GitHub's free tier. Works on any file type and the reassembly is byte-perfect, verified with SHA-256.

The 100 MiB chunk size corresponds to the binary interpretation (100 * 1024 * 1024 bytes) commonly used by GitHub's file size enforcement, even though GitHub documents it as "100 MB".

## How it works

- **`split:`** Scans the folder next to the tool and splits files over 100 MB into a `.split/` directory containing chunks and a `manifest.json`.
- **`assemble:`** Finds `.split/` directories and reassembles the original files, verifying their integrity.

## Configuration

The default chunk size is 100 MiB. To change it, create a `.gitsplit.toml` file in the **repository root** next to `.gitignore` and `.splitignore`:

```
my-repo/
├── .gitsplit.toml         <-- here
├── .gitignore
├── .splitignore
├── git-split/
│   └── target/release/git-split
└── ...
```

```toml
# .gitsplit.toml
chunk_size = 50
```

The value is in MiB. If `.gitsplit.toml` is missing or unreadable, the tool falls back to 100 MiB.

## Ignored files

Files matched by your repository's `.gitignore` are automatically skipped.

To add split-specific rules, create a `.splitignore` file in the **repository root** next to `.gitignore`. It uses the same syntax: folders, wildcards, and negation are all supported.

```
my-repo/
├── .gitignore           <-- here
├── .splitignore         <-- here too
├── git-split/
│   └── target/release/git-split
└── ...
```

```gitignore
# .splitignore

# Do not split ISO files
*.iso

# Do not split anything in backups/
backups/

# But allow this one specific file anyway
!backups/critical-dump.iso
```

## Building

```bash
cargo build --release
```

## Usage

Place the `git-split` folder inside your repository. It always operates on the folder one level up from the `git-split` repo root.

```
my-repo/
├── huge-file.bin          <-- 250 MB
├── git-split/             <-- tool lives here
│   └── target/release/git-split
└── ...
```

### Split files

```bash
# Run from inside the tool's folder
cd git-split
./target/release/git-split split
```

Result:

```
my-repo/
├── huge-file.bin.split/
│   ├── chunk_000
│   ├── chunk_001
│   ├── chunk_002
│   └── manifest.json
└── ...
```

### Assemble files

```bash
cd git-split
./target/release/git-split assemble
```

Result:

```
my-repo/
├── huge-file.bin          <-- restored, SHA-256 verified
└── ...
```

