# git-split

Split large files (>100 MiB) into smaller chunks for hosting on GitHub's free tier. Works on any file type and the reassembly is byte-perfect, verified with SHA-256.

The 100 MiB chunk size corresponds to the binary interpretation (100 * 1024 * 1024 bytes) commonly used by GitHub's file size enforcement, even though GitHub documents it as "100 MB".

## How it works

- **`split:`** Scans the folder next to the tool and splits files over the configured chunk size into a `.split/` directory containing chunks and a `manifest.json`.
- **`assemble:`** Finds `.split/` directories and reassembles the original files, verifying their integrity.

## Configuration

Create a `.gitsplit.toml` in the **repository root** to customize behavior. All settings are optional and default to values shown below.

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
chunk_size = 100         # MiB

[hooks]                  # all default to true
pre_commit     = true
post_commit    = true
post_checkout  = true
post_merge     = true
```

| Setting | Type | Default | Description |
|---|---|---|---|
| `chunk_size` | Integer | `100` | Chunk size in MiB. Files larger than this are split. |
| `hooks.pre_commit` | Boolean | `true` | Install `pre-commit` hook that auto-splits large files before committing. |
| `hooks.post_commit` | Boolean | `true` | Install `post-commit` hook that auto-assembles originals after committing. |
| `hooks.post_checkout` | Boolean | `true` | Install `post-checkout` hook that auto-assembles after branch switches or clone. |
| `hooks.post_merge` | Boolean | `true` | Install `post-merge` hook that auto-assembles after pull or merge. |

Omitting any key uses its default. Setting a hook to `false` prevents it from being installed when you run `git-split hooks --install`.

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

## Git-flow integration (optional)

By default the tool is entirely manual. If you prefer automatic splitting and assembling, you can install git hooks into **this specific repository only**:

```bash
cd git-split
./target/release/git-split hooks --install
```

This installs four hooks inside `.git/hooks/`:

| Hook | Trigger | What it does |
|---|---|---|
| `pre-commit` | Before every commit | Runs `split` so large files are chunked before the commit is created |
| `post-commit` | After every commit | Runs `assemble` so the original file is restored in the working tree for continued editing |
| `post-checkout` | After every `git checkout` or `git clone` | Runs `assemble` so teammates get the original files restored |
| `post-merge` | After every `git pull` or `git merge` | Runs `assemble` so pulled changes are restored |

These hooks are repo-local and do **not** affect any other repository on your machine.

To remove them later:

```bash
./target/release/git-split hooks --uninstall
```

### Windows note

Git hooks are shell scripts. They work on Windows if `sh.exe` is available (included with standard Git for Windows), but may not work with Git from the Microsoft Store or in environments without a POSIX shell.

