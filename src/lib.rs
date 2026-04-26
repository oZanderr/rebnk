//! Wwise `.bnk` soundbank parser and repacker.
//!
//! Handles both DIDX-indexed WEMs and WEMs only referenced from HIRC Sound
//! objects.  DIDX entries pointing past the DATA section (streamed/external
//! WEMs) are skipped safely instead of panicking.
//!
//! # Example
//! ```no_run
//! use rebnk::{parse_bnk, pack};
//! use std::collections::HashMap;
//! use std::path::Path;
//!
//! let bnk = parse_bnk(Path::new("Init.bnk")).unwrap();
//!
//! let mut replacements = HashMap::new();
//! replacements.insert(12345u32, std::fs::read("new_sound.wem").unwrap());
//!
//! pack(&bnk, &replacements, Path::new("out/Init.bnk")).unwrap();
//! ```

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// An embedded WEM audio entry.
#[derive(Debug, Clone)]
pub struct WemEntry {
    pub id: u32,
    pub offset: u32,
    pub size: u32,
    pub padding: u32,
    pub data: Vec<u8>,
    /// Whether this entry was found in the DIDX table.
    /// `false` means it was discovered via a HIRC Sound object.
    pub in_didx: bool,
}

/// An opaque BNK section (anything other than DIDX/DATA).
#[derive(Debug, Clone)]
pub struct RawSection {
    pub tag: [u8; 4],
    pub body: Vec<u8>,
}

/// Represents a section's position in the original file.
#[derive(Debug, Clone)]
pub enum Section {
    /// Opaque section, stored verbatim.
    Raw(RawSection),
    /// Marks where the DIDX + DATA pair appeared.
    WemData,
}

/// Parsed `.bnk` soundbank.
#[derive(Debug, Clone)]
pub struct BnkFile {
    pub name: String,
    pub wems: Vec<WemEntry>,
    /// Sections in original order.
    pub sections: Vec<Section>,
}

#[derive(Debug)]
pub enum WwiseError {
    Io(std::io::Error),
    InvalidFile(String),
}

impl std::fmt::Display for WwiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WwiseError::Io(e) => write!(f, "IO error: {e}"),
            WwiseError::InvalidFile(msg) => write!(f, "invalid BNK: {msg}"),
        }
    }
}

impl std::error::Error for WwiseError {}

impl From<std::io::Error> for WwiseError {
    fn from(e: std::io::Error) -> Self {
        WwiseError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, WwiseError>;

#[inline]
fn read_le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn write_section(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
}

/// Parse a `.bnk` file into a [`BnkFile`].
///
/// Sections are read sequentially.  DIDX + DATA are split into individual
/// [`WemEntry`] values; all other sections are stored as opaque blobs.
///
/// After DIDX parsing, the HIRC section is scanned for embedded Sound objects
/// (type 2, stream-type 0) whose source ID is not already in the DIDX.  These
/// "HIRC-only" WEMs are located in the DATA section by scanning for a RIFF
/// header whose size matches the expected `InMemoryMediaSize`.
///
/// DIDX entries whose offset+size extends past the DATA section (typically
/// streamed/external WEMs) are safely skipped.
pub fn parse_bnk(path: &Path) -> Result<BnkFile> {
    let raw = fs::read(path)?;
    parse_bnk_from_bytes(&raw, path)
}

/// Parse a BNK from an in-memory byte slice.
pub fn parse_bnk_from_bytes(raw: &[u8], path: &Path) -> Result<BnkFile> {
    let b = raw;
    let n = b.len();

    if n < 8 {
        return Err(WwiseError::InvalidFile("file too small".into()));
    }

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let mut bnk = BnkFile {
        name,
        wems: Vec::new(),
        sections: Vec::new(),
    };

    let mut didx_ids: HashSet<u32> = HashSet::new();
    let mut data_body_start: usize = 0;
    let mut data_body_size: usize = 0;
    let mut hirc_body_start: usize = 0;
    let mut hirc_body_size: usize = 0;

    let mut i = 0usize;

    while i + 8 <= n {
        let tag: [u8; 4] = b[i..i + 4].try_into().unwrap();
        let sec_sz = read_le_u32(b, i + 4) as usize;

        if i + 8 + sec_sz > n {
            return Err(WwiseError::InvalidFile(format!(
                "section {:?} at offset {i} extends past EOF",
                std::str::from_utf8(&tag).unwrap_or("????"),
            )));
        }

        if &tag == b"DIDX" {
            let num_entries = sec_sz / 12;

            let ds = i + 8 + sec_sz;
            if ds + 8 > n || &b[ds..ds + 4] != b"DATA" {
                return Err(WwiseError::InvalidFile(
                    "DATA section must follow DIDX".into(),
                ));
            }
            let data_sec_sz = read_le_u32(b, ds + 4) as usize;
            let db = ds + 8;
            data_body_start = db;
            data_body_size = data_sec_sz;

            let mut eo = i + 8;
            for e in 0..num_entries {
                let id = read_le_u32(b, eo);
                let off = read_le_u32(b, eo + 4);
                let sz = read_le_u32(b, eo + 8);

                let start = db + off as usize;
                let end = start + sz as usize;

                // Skip entries that point outside the DATA section
                // (streamed/external WEMs whose data is not in this file)
                if end <= n && (off as usize + sz as usize) <= data_sec_sz {
                    let padding = if e != num_entries - 1 && !sz.is_multiple_of(16) {
                        16 - sz % 16
                    } else {
                        0
                    };

                    didx_ids.insert(id);
                    bnk.wems.push(WemEntry {
                        id,
                        offset: off,
                        size: sz,
                        padding,
                        data: b[start..end].to_vec(),
                        in_didx: true,
                    });
                }

                eo += 12;
            }

            bnk.sections.push(Section::WemData);
            i = ds + 8 + data_sec_sz;
        } else {
            if &tag == b"HIRC" {
                hirc_body_start = i + 8;
                hirc_body_size = sec_sz;
            }
            bnk.sections.push(Section::Raw(RawSection {
                tag,
                body: b[i + 8..i + 8 + sec_sz].to_vec(),
            }));
            i += 8 + sec_sz;
        }
    }

    // ── HIRC scan: discover embedded WEMs not in DIDX ────────────────────
    if hirc_body_size > 0 && data_body_size > 0 && hirc_body_start + 4 <= n {
        let num_objects = read_le_u32(b, hirc_body_start) as usize;
        let mut pos = hirc_body_start + 4;

        for _ in 0..num_objects {
            if pos + 5 > n {
                break;
            }
            let obj_type = b[pos];
            let obj_size = read_le_u32(b, pos + 1) as usize;
            let obj_body = pos + 9;
            let obj_end = pos + 5 + obj_size;

            // Type 2 = Sound SFX
            if obj_type == 2 && obj_end <= n && obj_body + 13 <= n {
                let stream_type = b[obj_body + 4];
                let source_id = read_le_u32(b, obj_body + 5);
                let file_size = read_le_u32(b, obj_body + 9);

                // stream_type 0 = embedded data, and not already found in DIDX
                if stream_type == 0
                    && file_size > 0
                    && !didx_ids.contains(&source_id)
                    && let Some(offset) =
                        find_wem_in_data(b, data_body_start, data_body_size, file_size)
                {
                    didx_ids.insert(source_id);
                    bnk.wems.push(WemEntry {
                        id: source_id,
                        offset: offset as u32,
                        size: file_size,
                        padding: if !file_size.is_multiple_of(16) {
                            16 - file_size % 16
                        } else {
                            0
                        },
                        data: b[data_body_start + offset
                            ..data_body_start + offset + file_size as usize]
                            .to_vec(),
                        in_didx: false,
                    });
                }
            }

            pos = pos + 5 + obj_size;
        }
    }

    Ok(bnk)
}

/// Scan the DATA section for a WEM (RIFF header) at 16-byte aligned positions
/// whose total size matches `expected_size`.
fn find_wem_in_data(
    b: &[u8],
    data_body_start: usize,
    data_body_size: usize,
    expected_size: u32,
) -> Option<usize> {
    let data_end = data_body_start + data_body_size;
    let expected = expected_size as usize;

    let mut pos = data_body_start;
    while pos + expected <= data_end && pos + 12 <= b.len() {
        if &b[pos..pos + 4] == b"RIFF" {
            let riff_size = read_le_u32(b, pos + 4) as usize;
            if riff_size + 8 == expected {
                return Some(pos - data_body_start);
            }
        }
        pos += 16;
    }
    None
}

/// Extract all WEM streams to `out_root/<name>/`.
pub fn extract(bnk: &BnkFile, out_root: &Path) -> Result<()> {
    let dir = out_root.join(&bnk.name);
    fs::create_dir_all(&dir)?;

    for w in &bnk.wems {
        fs::write(dir.join(format!("{}.wem", w.id)), &w.data)?;
    }

    Ok(())
}

/// Repack a soundbank with replacement WEM data into an in-memory buffer.
///
/// Same semantics as [`pack`], but returns the rebuilt BNK bytes instead of
/// writing them to disk. Use this when the caller plans to feed the bytes
/// straight into another writer (e.g. a pak builder) and wants to avoid an
/// intermediate temp file.
pub fn pack_to_bytes(bnk: &BnkFile, replacements: &HashMap<u32, Vec<u8>>) -> Result<Vec<u8>> {
    let last_idx = bnk.wems.len().saturating_sub(1);

    let mut data_body: Vec<u8> = Vec::new();
    let mut didx_body: Vec<u8> = Vec::with_capacity(bnk.wems.len() * 12);

    for (i, w) in bnk.wems.iter().enumerate() {
        let wem_data = replacements
            .get(&w.id)
            .map(|v| v.as_slice())
            .unwrap_or(&w.data);
        let size = wem_data.len() as u32;

        let offset = data_body.len() as u32;
        data_body.extend_from_slice(wem_data);

        let padding = if i != last_idx && !size.is_multiple_of(16) {
            16 - size % 16
        } else {
            0
        };
        if padding > 0 {
            data_body.extend(std::iter::repeat_n(0u8, padding as usize));
        }

        didx_body.extend_from_slice(&w.id.to_le_bytes());
        didx_body.extend_from_slice(&offset.to_le_bytes());
        didx_body.extend_from_slice(&size.to_le_bytes());
    }

    let mut out: Vec<u8> = Vec::new();

    for section in &bnk.sections {
        match section {
            Section::WemData => {
                if !didx_body.is_empty() {
                    write_section(&mut out, b"DIDX", &didx_body);
                    write_section(&mut out, b"DATA", &data_body);
                }
            }
            Section::Raw(s) => {
                write_section(&mut out, &s.tag, &s.body);
            }
        }
    }

    Ok(out)
}

/// Repack a soundbank with replacement WEM data and write to disk.
///
/// `replacements` maps WEM IDs to new audio bytes.  Entries without a
/// replacement keep their original data.  Sections are written in their
/// original order.
///
/// WEMs that were discovered via HIRC (not originally in the DIDX) are added
/// to the DIDX in the output so the repacked BNK is self-consistent.
pub fn pack(bnk: &BnkFile, replacements: &HashMap<u32, Vec<u8>>, output_path: &Path) -> Result<()> {
    let bytes = pack_to_bytes(bnk, replacements)?;

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output_path, &bytes)?;

    Ok(())
}
