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

/// Wwise property ids for the baked low/high-pass filter cutoffs.
const PROP_LPF: u8 = 3;
const PROP_HPF: u8 = 4;

/// Zero the baked LPF/HPF properties on every `CAkSound` whose source id is in `source_ids`, so
/// replaced audio plays without the always-on low/high-pass those sounds carry.
///
/// Operates in place on the HIRC section. LPF/HPF values are 4-byte unions, so this changes no
/// object sizes (no reserialization). Returns the number of LPF/HPF values zeroed. Sounds with a
/// layout this does not understand (effect slots, inline source media) are skipped.
///
/// Targets the Wwise bank version 145 `CAkSound` layout used by Marvel Rivals.
pub fn clear_sound_filters(bnk: &mut BnkFile, source_ids: &HashSet<u32>) -> usize {
    for sec in &mut bnk.sections {
        if let Section::Raw(rs) = sec
            && &rs.tag == b"HIRC"
        {
            return clear_filters_in_hirc(&mut rs.body, source_ids);
        }
    }
    0
}

fn clear_filters_in_hirc(body: &mut [u8], source_ids: &HashSet<u32>) -> usize {
    let n = body.len();
    if n < 4 {
        return 0;
    }
    let num_objects = read_le_u32(body, 0) as usize;
    let mut pos = 4usize;
    let mut cleared = 0usize;

    for _ in 0..num_objects {
        if pos + 5 > n {
            break;
        }
        let obj_type = body[pos];
        let obj_size = read_le_u32(body, pos + 1) as usize;
        let obj_body = pos + 9; // after type(1) + size(4) + ulID(4)
        let obj_end = pos + 5 + obj_size;
        if obj_end > n {
            break;
        }

        // Type 2 = CAkSound; sourceID is at obj_body + 5 (after ulPluginID + StreamType).
        if obj_type == 2 && obj_body + 9 <= n {
            let source_id = read_le_u32(body, obj_body + 5);
            if source_ids.contains(&source_id) {
                cleared += zero_sound_filter_props(body, obj_body, obj_end);
            }
        }

        pos = obj_end;
    }
    cleared
}

/// Walk a `CAkSound`'s `NodeBaseParams` to the `NodeInitialParams` prop bundle and zero any baked
/// LPF/HPF values. Returns the count zeroed; returns 0 on any layout it does not recognize.
fn zero_sound_filter_props(body: &mut [u8], obj_body: usize, obj_end: usize) -> usize {
    let mut pos = obj_body;

    // AkBankSourceData: ulPluginID(4) StreamType(1) sourceID(4) uInMemoryMediaSize(4) uSourceBits(1)
    if pos + 14 > obj_end {
        return 0;
    }
    if body[pos + 13] & 0x80 != 0 {
        return 0; // bHasSource: inline source plugin data, unsupported layout
    }
    pos += 14;

    // NodeInitialFxParams: bIsOverrideParentFX(1) uNumFx(1) [+ fx list when uNumFx > 0]
    if pos + 2 > obj_end {
        return 0;
    }
    pos += 1;
    let num_fx = body[pos];
    pos += 1;
    if num_fx != 0 {
        return 0; // has effect slots; skip rather than risk mis-parsing
    }

    // Metadata fx: bIsOverrideParentMetadata(1) uNumFx(1) [+ fx list when uNumFx > 0]
    if pos + 2 > obj_end {
        return 0;
    }
    pos += 1;
    let num_fx_meta = body[pos];
    pos += 1;
    if num_fx_meta != 0 {
        return 0;
    }

    // bOverrideAttachmentParams(1) OverrideBusId(4) DirectParentID(4) byBitVector(1)
    pos += 10;

    // NodeInitialParams AkPropBundle: cProps(1) + cProps*pID(1) + cProps*pValue(4)
    if pos + 1 > obj_end {
        return 0;
    }
    let cprops = body[pos] as usize;
    pos += 1;
    let pid_array = pos;
    let pvalue_array = pos + cprops;
    if pvalue_array + cprops * 4 > obj_end {
        return 0;
    }

    let mut cleared = 0;
    for i in 0..cprops {
        let pid = body[pid_array + i];
        if pid == PROP_LPF || pid == PROP_HPF {
            let vo = pvalue_array + i * 4;
            body[vo..vo + 4].copy_from_slice(&0f32.to_le_bytes());
            cleared += 1;
        }
    }
    cleared
}

/// Make every `CAkSound` whose sourceID is in `source_ids` ignore the effects it would otherwise
/// inherit from its parent actor-mixer chain, by setting `bIsOverrideParentFX` so the sound uses
/// its own (empty) effect list. Size-neutral (flips one byte). Sounds that carry their own effects
/// (`uNumFx > 0`) or an inline source are left untouched. Returns the number changed.
///
/// Targets the Wwise bank version 145 `CAkSound` layout used by Marvel Rivals.
pub fn override_parent_fx(bnk: &mut BnkFile, source_ids: &HashSet<u32>) -> usize {
    for sec in &mut bnk.sections {
        if let Section::Raw(rs) = sec
            && &rs.tag == b"HIRC"
        {
            return override_parent_fx_in_hirc(&mut rs.body, source_ids);
        }
    }
    0
}

fn override_parent_fx_in_hirc(body: &mut [u8], source_ids: &HashSet<u32>) -> usize {
    let n = body.len();
    if n < 4 {
        return 0;
    }
    let num_objects = read_le_u32(body, 0) as usize;
    let mut pos = 4usize;
    let mut changed = 0usize;

    for _ in 0..num_objects {
        if pos + 9 > n {
            break;
        }
        let obj_type = body[pos];
        let obj_size = read_le_u32(body, pos + 1) as usize;
        let obj_body = pos + 9;
        let obj_end = pos + 5 + obj_size;
        if obj_end > n {
            break;
        }

        // CAkSound NodeInitialFxParams: bIsOverrideParentFX at obj_body+14, uNumFx at obj_body+15.
        if obj_type == 2
            && obj_body + 16 <= obj_end
            && body[obj_body + 13] & 0x80 == 0
            && body[obj_body + 15] == 0
            && source_ids.contains(&read_le_u32(body, obj_body + 5))
        {
            body[obj_body + 14] = 1;
            changed += 1;
        }

        pos = obj_end;
    }
    changed
}

/// Byte offset where a HIRC object's `NodeBaseParams` begins, or `None` for a type this does not
/// parse. `CAkSound` (2) prefixes it with a 14-byte `AkBankSourceData`; containers and actor-mixers
/// (`CAkRanSeqCntr` 5, `CAkSwitchCntr` 6, `CAkActorMixer` 7, `CAkLayerCntr` 9) start it right after
/// `ulID`.
fn node_base_start(body: &[u8], obj_type: u8, obj_body: usize, obj_end: usize) -> Option<usize> {
    match obj_type {
        2 if obj_body + 14 <= obj_end && body[obj_body + 13] & 0x80 == 0 => Some(obj_body + 14),
        5 | 6 | 7 | 9 => Some(obj_body),
        _ => None,
    }
}

/// A node's resolved bus routing and the byte offset of its `OverrideBusId` field in the HIRC body.
struct NodeBus {
    bus_offset: usize,
    override_bus: u32,
    direct_parent: u32,
}

/// Walk a `NodeBaseParams` from `start` (the byte after the node's type-specific prefix) to its
/// `OverrideBusId`/`DirectParentID`. Returns `None` on a layout this does not understand.
fn read_node_bus(body: &[u8], start: usize, end: usize) -> Option<NodeBus> {
    let mut pos = start;

    // NodeInitialFxParams: bIsOverrideParentFX(1) uNumFx(1) [bitsFXBypass(1) + uNumFx*7]
    if pos + 2 > end {
        return None;
    }
    pos += 1;
    let num_fx = body[pos];
    pos += 1;
    if num_fx > 0 {
        pos += 1 + num_fx as usize * 7;
    }

    // Metadata fx: bIsOverrideParentMetadata(1) uNumFx(1) [+ list]
    if pos + 2 > end {
        return None;
    }
    pos += 1;
    let num_fx_meta = body[pos];
    pos += 1;
    if num_fx_meta != 0 {
        return None;
    }

    // bOverrideAttachmentParams(1) OverrideBusId(4) DirectParentID(4)
    if pos + 9 > end {
        return None;
    }
    pos += 1;
    let bus_offset = pos;
    let override_bus = read_le_u32(body, pos);
    let direct_parent = read_le_u32(body, pos + 4);
    Some(NodeBus {
        bus_offset,
        override_bus,
        direct_parent,
    })
}

/// Re-point every `CAkSound` whose sourceID is in `source_ids` from `from_bus` to `to_bus`, but
/// only when the sound currently resolves to `from_bus` (directly or up its parent chain).
///
/// This lets a replaced sound bypass always-on effects living on `from_bus` (e.g. an announcer
/// EQ/compressor) by routing it to a cleaner ancestor bus instead. Size-neutral (rewrites the
/// 4-byte `OverrideBusId` in place). Sounds that resolve to any other bus are left untouched, so a
/// changed bank hierarchy is never mis-wired. Returns the number of sounds rerouted.
pub fn reroute_sound_bus(
    bnk: &mut BnkFile,
    source_ids: &HashSet<u32>,
    from_bus: u32,
    to_bus: u32,
) -> usize {
    for sec in &mut bnk.sections {
        if let Section::Raw(rs) = sec
            && &rs.tag == b"HIRC"
        {
            return reroute_in_hirc(&mut rs.body, source_ids, from_bus, to_bus);
        }
    }
    0
}

fn reroute_in_hirc(
    body: &mut [u8],
    source_ids: &HashSet<u32>,
    from_bus: u32,
    to_bus: u32,
) -> usize {
    let n = body.len();
    if n < 4 {
        return 0;
    }
    let num_objects = read_le_u32(body, 0) as usize;

    // Pass 1: map node id -> (override_bus, direct_parent) for the node types that can route or
    // parent a sound, and remember each target sound's OverrideBusId offset for rewriting.
    let mut nodes: HashMap<u32, (u32, u32)> = HashMap::new();
    let mut targets: Vec<(u32, usize)> = Vec::new();
    let mut pos = 4usize;

    for _ in 0..num_objects {
        if pos + 9 > n {
            break;
        }
        let obj_type = body[pos];
        let obj_size = read_le_u32(body, pos + 1) as usize;
        let obj_body = pos + 9;
        let obj_end = pos + 5 + obj_size;
        if obj_end > n {
            break;
        }
        let ul_id = read_le_u32(body, pos + 5);

        if let Some(start) = node_base_start(body, obj_type, obj_body, obj_end)
            && let Some(nb) = read_node_bus(body, start, obj_end)
        {
            nodes.insert(ul_id, (nb.override_bus, nb.direct_parent));
            if obj_type == 2 && source_ids.contains(&read_le_u32(body, obj_body + 5)) {
                targets.push((ul_id, nb.bus_offset));
            }
        }

        pos = obj_end;
    }

    // Pass 2: reroute each target sound whose resolved bus is the one we mean to bypass.
    let mut rerouted = 0;
    for (id, bus_offset) in targets {
        if resolve_bus(&nodes, id) == Some(from_bus) {
            body[bus_offset..bus_offset + 4].copy_from_slice(&to_bus.to_le_bytes());
            rerouted += 1;
        }
    }
    rerouted
}

/// Resolve a node's effective output bus by following `DirectParentID` until a non-zero
/// `OverrideBusId` is found. `None` if the chain leaves the bank or loops.
fn resolve_bus(nodes: &HashMap<u32, (u32, u32)>, start: u32) -> Option<u32> {
    let mut cur = start;
    let mut seen = HashSet::new();
    while seen.insert(cur) {
        let (override_bus, parent) = nodes.get(&cur).copied()?;
        if override_bus != 0 {
            return Some(override_bus);
        }
        if parent == 0 {
            return None;
        }
        cur = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a one-object HIRC body holding a version-145 `CAkSound` with `source_id` and a
    /// `NodeInitialParams` prop bundle carrying Volume(0)=1.0, LPF(3)=15.0, HPF(4)=35.0.
    fn hirc_with_sound(source_id: u32) -> Vec<u8> {
        let mut obj = Vec::new();
        obj.extend_from_slice(&1000u32.to_le_bytes()); // ulID
        // AkBankSourceData
        obj.extend_from_slice(&0x0004_0001u32.to_le_bytes()); // ulPluginID (VORBIS)
        obj.push(0); // StreamType: Data/bnk
        obj.extend_from_slice(&source_id.to_le_bytes()); // sourceID
        obj.extend_from_slice(&999u32.to_le_bytes()); // uInMemoryMediaSize
        obj.push(0); // uSourceBits (bHasSource = 0)
        // NodeBaseParams
        obj.push(0); // bIsOverrideParentFX
        obj.push(0); // uNumFx
        obj.push(0); // bIsOverrideParentMetadata
        obj.push(0); // uNumFx (metadata)
        obj.push(0); // bOverrideAttachmentParams
        obj.extend_from_slice(&0u32.to_le_bytes()); // OverrideBusId
        obj.extend_from_slice(&0u32.to_le_bytes()); // DirectParentID
        obj.push(0); // byBitVector
        // NodeInitialParams AkPropBundle
        obj.push(3); // cProps
        obj.extend_from_slice(&[0u8, PROP_LPF, PROP_HPF]); // pID array
        obj.extend_from_slice(&1.0f32.to_le_bytes()); // Volume
        obj.extend_from_slice(&15.0f32.to_le_bytes()); // LPF
        obj.extend_from_slice(&35.0f32.to_le_bytes()); // HPF

        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // num objects
        body.push(2); // CAkSound
        body.extend_from_slice(&(obj.len() as u32).to_le_bytes()); // dwSectionSize
        body.extend_from_slice(&obj);
        body
    }

    fn bnk_with_hirc(body: Vec<u8>) -> BnkFile {
        BnkFile {
            name: "test".into(),
            wems: Vec::new(),
            sections: vec![Section::Raw(RawSection {
                tag: *b"HIRC",
                body,
            })],
        }
    }

    fn props(bnk: &BnkFile) -> [f32; 3] {
        let Section::Raw(rs) = &bnk.sections[0] else {
            panic!("expected HIRC raw section");
        };
        let n = rs.body.len();
        let base = n - 12; // three f32 pValues at the tail
        [
            f32::from_le_bytes(rs.body[base..base + 4].try_into().unwrap()),
            f32::from_le_bytes(rs.body[base + 4..base + 8].try_into().unwrap()),
            f32::from_le_bytes(rs.body[base + 8..base + 12].try_into().unwrap()),
        ]
    }

    #[test]
    fn zeroes_lpf_hpf_for_targeted_source() {
        let mut bnk = bnk_with_hirc(hirc_with_sound(12345));
        let cleared = clear_sound_filters(&mut bnk, &HashSet::from([12345u32]));
        assert_eq!(cleared, 2);
        // Volume untouched; LPF and HPF zeroed.
        assert_eq!(props(&bnk), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn leaves_untargeted_source_untouched() {
        let mut bnk = bnk_with_hirc(hirc_with_sound(12345));
        let cleared = clear_sound_filters(&mut bnk, &HashSet::from([99999u32]));
        assert_eq!(cleared, 0);
        assert_eq!(props(&bnk), [1.0, 15.0, 35.0]);
    }

    fn node_base_bytes(override_bus: u32, direct_parent: u32) -> Vec<u8> {
        // bIsOverrideParentFX, uNumFx, bIsOverrideParentMetadata, uNumFx, bOverrideAttachmentParams
        let mut v = vec![0u8; 5];
        v.extend_from_slice(&override_bus.to_le_bytes()); // OverrideBusId
        v.extend_from_slice(&direct_parent.to_le_bytes()); // DirectParentID
        v
    }

    fn wrap_obj(obj_type: u8, inner: Vec<u8>) -> Vec<u8> {
        let mut obj = vec![obj_type];
        obj.extend_from_slice(&(inner.len() as u32).to_le_bytes()); // dwSectionSize
        obj.extend_from_slice(&inner);
        obj
    }

    /// A streamed `CAkSound` with no baked props, routed via its parent.
    fn sound_obj(ul_id: u32, source_id: u32, override_bus: u32, parent: u32) -> Vec<u8> {
        let mut inner = Vec::new();
        inner.extend_from_slice(&ul_id.to_le_bytes());
        inner.extend_from_slice(&0x0004_0001u32.to_le_bytes()); // ulPluginID
        inner.push(1); // StreamType: PrefetchStreaming
        inner.extend_from_slice(&source_id.to_le_bytes());
        inner.extend_from_slice(&0u32.to_le_bytes()); // uInMemoryMediaSize
        inner.push(9); // uSourceBits (bHasSource = 0)
        inner.extend_from_slice(&node_base_bytes(override_bus, parent));
        wrap_obj(2, inner)
    }

    /// A container/actor-mixer node (NodeBaseParams right after ulID): type 5/6/7/9.
    fn container_obj(obj_type: u8, ul_id: u32, override_bus: u32, parent: u32) -> Vec<u8> {
        let mut inner = ul_id.to_le_bytes().to_vec();
        inner.extend_from_slice(&node_base_bytes(override_bus, parent));
        wrap_obj(obj_type, inner)
    }

    fn mixer_obj(ul_id: u32, override_bus: u32, parent: u32) -> Vec<u8> {
        container_obj(7, ul_id, override_bus, parent)
    }

    fn hirc_body(objs: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(objs.len() as u32).to_le_bytes());
        for o in objs {
            body.extend_from_slice(o);
        }
        body
    }

    /// The sound is the first object, so its OverrideBusId lands at body offset 32.
    fn sound_bus(bnk: &BnkFile) -> u32 {
        let Section::Raw(rs) = &bnk.sections[0] else {
            panic!("expected HIRC raw section");
        };
        u32::from_le_bytes(rs.body[32..36].try_into().unwrap())
    }

    #[test]
    fn reroutes_sound_resolving_to_source_bus() {
        let (from_bus, to_bus) = (3919227308u32, 812276737u32);
        let sound = sound_obj(277881354, 980924621, 0, 29931845);
        let mixer = mixer_obj(29931845, from_bus, 0);
        let mut bnk = bnk_with_hirc(hirc_body(&[sound, mixer]));
        let n = reroute_sound_bus(&mut bnk, &HashSet::from([980924621u32]), from_bus, to_bus);
        assert_eq!(n, 1);
        assert_eq!(sound_bus(&bnk), to_bus);
    }

    #[test]
    fn leaves_sound_on_other_bus_untouched() {
        let (from_bus, to_bus) = (3919227308u32, 812276737u32);
        let sound = sound_obj(277881354, 980924621, 0, 29931845);
        let mixer = mixer_obj(29931845, 111u32, 0); // resolves to a different bus
        let mut bnk = bnk_with_hirc(hirc_body(&[sound, mixer]));
        let n = reroute_sound_bus(&mut bnk, &HashSet::from([980924621u32]), from_bus, to_bus);
        assert_eq!(n, 0);
        assert_eq!(sound_bus(&bnk), 0); // unchanged
    }

    #[test]
    fn reroutes_through_switch_container_chain() {
        // The battle hit-sounds route sound -> CAkSwitchCntr -> CAkActorMixer -> bus.
        let (from_bus, to_bus) = (1952531228u32, 2791637696u32);
        let sound = sound_obj(181235746, 975983943, 0, 306721962);
        let switch = container_obj(6, 306721962, 0, 576717991);
        let mixer = mixer_obj(576717991, from_bus, 0);
        let mut bnk = bnk_with_hirc(hirc_body(&[sound, switch, mixer]));
        let n = reroute_sound_bus(&mut bnk, &HashSet::from([975983943u32]), from_bus, to_bus);
        assert_eq!(n, 1);
        assert_eq!(sound_bus(&bnk), to_bus);
    }

    /// In `hirc_with_sound`, the single sound is the first object, so bIsOverrideParentFX (the byte
    /// after its 14-byte AkBankSourceData) lands at body offset 27.
    fn override_fx_flag(bnk: &BnkFile) -> u8 {
        let Section::Raw(rs) = &bnk.sections[0] else {
            panic!("expected HIRC raw section");
        };
        rs.body[27]
    }

    #[test]
    fn override_parent_fx_sets_flag_for_target() {
        let mut bnk = bnk_with_hirc(hirc_with_sound(12345));
        assert_eq!(override_fx_flag(&bnk), 0);
        let n = override_parent_fx(&mut bnk, &HashSet::from([12345u32]));
        assert_eq!(n, 1);
        assert_eq!(override_fx_flag(&bnk), 1);
    }

    #[test]
    fn override_parent_fx_skips_non_target() {
        let mut bnk = bnk_with_hirc(hirc_with_sound(12345));
        let n = override_parent_fx(&mut bnk, &HashSet::from([99999u32]));
        assert_eq!(n, 0);
        assert_eq!(override_fx_flag(&bnk), 0);
    }
}
