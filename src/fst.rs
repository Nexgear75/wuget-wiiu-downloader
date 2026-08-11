//! Binary layouts of the two metadata blobs a NUS dump is built around: the
//! TMD (title metadata, `title.tmd`) and the FST (file system table, which
//! lives inside the first decrypted content).
//!
//! Offsets are transcribed from cdecrypt's `TitleMetaData` / `FST` / `FEntry`
//! structs (see `reference/cdecrypt.c`).

use anyhow::{Result, bail, ensure};

pub const FST_MAGIC: u32 = 0x4653_5400; // "FST\0"

/// Offset of the content table inside a TMD.
const CONTENTS_OFFSET: usize = 0xB04;
const CONTENT_ENTRY_SIZE: usize = 0x30;

fn be16(buf: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([buf[at], buf[at + 1]])
}

fn be32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn be64(buf: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[at..at + 8]);
    u64::from_be_bytes(b)
}

/// Which common key the title was encrypted against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Retail,
    Dev,
}

#[derive(Debug, Clone, Copy)]
pub struct Content {
    pub id: u32,
    pub index: u16,
    /// Bit 1 (`0x02`) means the content carries an H0..H3 hash tree.
    pub kind: u16,
    pub size: u64,
}

impl Content {
    pub fn is_hashed(&self) -> bool {
        self.kind & 0x02 != 0
    }

    /// The `.app` file this content is stored in.
    pub fn file_name(&self) -> String {
        format!("{:08x}.app", self.id)
    }
}

#[derive(Debug, Clone)]
pub struct Tmd {
    pub title_id: [u8; 8],
    pub title_version: [u8; 2],
    pub key_kind: KeyKind,
    pub contents: Vec<Content>,
}

impl Tmd {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        ensure!(
            buf.len() >= CONTENTS_OFFSET,
            "TMD tronqué : {} octets", buf.len()
        );

        let version = buf[0x180];
        ensure!(version == 1, "version de TMD non supportée : {version}");

        // The issuer decides which common key unwraps the title key.
        let issuer = &buf[0x140..0x180];
        let issuer = issuer.split(|&b| b == 0).next().unwrap_or_default();
        let key_kind = match issuer {
            b"Root-CA00000003-CP0000000b" => KeyKind::Retail,
            b"Root-CA00000004-CP00000010" => KeyKind::Dev,
            other => bail!(
                "émetteur de TMD inconnu : {:?}",
                String::from_utf8_lossy(other)
            ),
        };

        let content_count = be16(buf, 0x1DE) as usize;
        let needed = CONTENTS_OFFSET + content_count * CONTENT_ENTRY_SIZE;
        ensure!(
            buf.len() >= needed,
            "TMD annonce {content_count} contenus mais ne fait que {} octets",
            buf.len()
        );

        let contents = (0..content_count)
            .map(|i| {
                let at = CONTENTS_OFFSET + i * CONTENT_ENTRY_SIZE;
                Content {
                    id: be32(buf, at),
                    index: be16(buf, at + 0x04),
                    kind: be16(buf, at + 0x06),
                    size: be64(buf, at + 0x08),
                }
            })
            .collect();

        Ok(Tmd {
            title_id: buf[0x18C..0x194].try_into().unwrap(),
            title_version: buf[0x1DC..0x1DE].try_into().unwrap(),
            key_kind,
            contents,
        })
    }

    pub fn total_size(&self) -> u64 {
        self.contents.iter().map(|c| c.size).sum()
    }
}

/// One file the FST says to extract.
#[derive(Debug, Clone)]
pub struct FstFile {
    /// Path relative to the output root, e.g. `content/Common/Stage/foo.szs`.
    pub path: String,
    /// Byte offset within the (decrypted) content stream.
    pub offset: u64,
    pub length: u64,
    /// Index into `Tmd::contents`.
    pub content_index: usize,
}

/// Walk the FST and return every file entry, in FST order.
///
/// The traversal mirrors cdecrypt's level stack: a directory entry pushes its
/// `next_offset` as the index at which the level ends.
pub fn parse(fst: &[u8]) -> Result<Vec<FstFile>> {
    ensure!(fst.len() >= 0x20, "FST tronqué");
    ensure!(
        be32(fst, 0) == FST_MAGIC,
        "magic FST inattendu : {:#010x}",
        be32(fst, 0)
    );

    let fst_info_count = be32(fst, 0x08) as usize;
    let entry_base = 0x20 + fst_info_count * 0x20;
    ensure!(
        fst.len() >= entry_base + 0x10,
        "FST trop court pour {fst_info_count} FSTInfo"
    );

    // The root entry's "next offset" field doubles as the total entry count.
    let entry_count = be32(fst, entry_base + 8) as usize;
    let names_base = entry_base + entry_count * 0x10;
    ensure!(
        fst.len() >= names_base,
        "FST trop court pour {entry_count} entrées"
    );

    let name_at = |offset: usize| -> String {
        let start = names_base + offset;
        let end = fst[start..]
            .iter()
            .position(|&b| b == 0)
            .map(|n| start + n)
            .unwrap_or(fst.len());
        String::from_utf8_lossy(&fst[start..end]).into_owned()
    };

    let mut files = Vec::new();
    // Directory names for the current path, and the entry index each level ends at.
    let mut stack: Vec<(String, usize)> = Vec::new();

    for i in 1..entry_count {
        let at = entry_base + i * 0x10;
        let type_name = be32(fst, at);
        let entry_type = (type_name >> 24) as u8;
        let name_offset = (type_name & 0x00FF_FFFF) as usize;
        let flags = be16(fst, at + 0x0C);
        let content_index = be16(fst, at + 0x0E) as usize;

        // Leave any directories whose range ended before this entry.
        while stack.last().is_some_and(|&(_, end)| end == i) {
            stack.pop();
        }

        if entry_type & 1 != 0 {
            // Directory: its range runs until `next_offset`.
            let next_offset = be32(fst, at + 0x08) as usize;
            stack.push((name_at(name_offset), next_offset));
            ensure!(stack.len() < 16, "arborescence FST trop profonde");
            continue;
        }

        // 0x80 marks entries that carry no data of their own.
        if entry_type & 0x80 != 0 {
            continue;
        }

        let mut offset = be32(fst, at + 0x04) as u64;
        if flags & 4 == 0 {
            offset <<= 5;
        }

        let mut path = String::new();
        for (dir, _) in &stack {
            path.push_str(dir);
            path.push('/');
        }
        path.push_str(&name_at(name_offset));

        files.push(FstFile {
            path,
            offset,
            length: be32(fst, at + 0x08) as u64,
            content_index,
        });
    }

    Ok(files)
}
