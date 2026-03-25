# rebnk

A Rust library for parsing, extracting, and repacking Wwise `.bnk` soundbank files. Built for replacing embedded WEM audio streams inside sound banks.

## What it does

- Parses `.bnk` files into structured data (WEM entries + opaque sections)
- Extracts individual `.wem` audio files from a sound bank
- Repacks a sound bank with replacement WEM data, preserving all non-audio sections and original section order

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
rebnk = { path = "../rebnk" }
```

### Parse a sound bank

```rust
use rebnk::parse_bnk;
use std::path::Path;

let bnk = parse_bnk(Path::new("Init.bnk")).unwrap();

println!("{} WEM entries found", bnk.wems.len());
for w in &bnk.wems {
    println!("  id={} size={}", w.id, w.size);
}
```

### Extract WEM files

```rust
use rebnk::{parse_bnk, extract};
use std::path::Path;

let bnk = parse_bnk(Path::new("Init.bnk")).unwrap();
extract(&bnk, Path::new("output/")).unwrap();
// writes output/Init/<id>.wem for each embedded stream
```

### Replace WEM entries and repack

```rust
use rebnk::{parse_bnk, pack};
use std::collections::HashMap;
use std::path::Path;

let bnk = parse_bnk(Path::new("Init.bnk")).unwrap();

let mut replacements = HashMap::new();
replacements.insert(12345, std::fs::read("my_sound.wem").unwrap());
replacements.insert(67890, std::fs::read("another.wem").unwrap());

pack(&bnk, &replacements, Path::new("out/Init.bnk")).unwrap();
// WEM IDs not in the map keep their original data
```

## API

| Function | Description |
|---|---|
| `parse_bnk(path)` | Parse a `.bnk` file into a `BnkFile` |
| `extract(bnk, out_root)` | Extract all WEM streams to `out_root/<name>/` |
| `pack(bnk, replacements, output_path)` | Repack with optional WEM replacements |

## How it works

A `.bnk` file is a sequence of sections, each with a 4-byte ASCII tag and a 4-byte little-endian length. The library parses two sections deeply:

- **DIDX** (Data Index) -- array of 12-byte entries mapping WEM IDs to offsets/sizes in the DATA section
- **DATA** -- concatenated WEM audio streams with alignment padding

All other sections (BKHD, HIRC, STID, etc.) are stored as opaque blobs and written back verbatim during repacking. This means the library can only replace existing WEM entries, not add or remove them, since other sections (like HIRC) reference WEM IDs internally.
