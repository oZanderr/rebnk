# rebnk

A Rust library for parsing, extracting, and repacking Wwise `.bnk` soundbank files. Built for replacing embedded WEM audio streams inside sound banks.

## What it does

- Parses `.bnk` files into structured data (WEM entries + opaque sections)
- Discovers embedded WEMs referenced by HIRC Sound objects even when missing from DIDX
- Safely skips DIDX entries that point outside the DATA section (streamed/external media)
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

### Parse from in-memory bytes

```rust
use rebnk::parse_bnk_from_bytes;
use std::path::Path;

let raw = std::fs::read("Init.bnk").unwrap();
let bnk = parse_bnk_from_bytes(&raw, Path::new("Init.bnk")).unwrap();

println!("{} WEM entries found", bnk.wems.len());
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
| `parse_bnk_from_bytes(raw, path)` | Parse BNK data from an in-memory byte slice |
| `extract(bnk, out_root)` | Extract all WEM streams to `out_root/<name>/` |
| `pack(bnk, replacements, output_path)` | Repack with optional WEM replacements |

## How it works

A `.bnk` file is a sequence of sections, each with a 4-byte ASCII tag and a 4-byte little-endian length. The library parses DIDX and DATA deeply, then scans HIRC Sound objects to discover additional embedded streams:

- **DIDX** (Data Index) -- array of 12-byte entries mapping WEM IDs to offsets/sizes in the DATA section
- **DATA** -- concatenated WEM audio streams with alignment padding
- **HIRC Sound objects** (type 2, stream type 0) -- source IDs and in-memory media sizes used to find embedded RIFF/WEM data not listed in DIDX

All other sections (BKHD, STID, etc.) are stored as opaque blobs and written back verbatim during repacking. Repacking can replace any discovered embedded WEM entry (including HIRC-only ones). HIRC-only entries are added to the output DIDX so the rebuilt bank remains self-consistent. The library still does not rewrite HIRC content itself, so it does not create brand new logical sound references.
