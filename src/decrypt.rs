//! Turning an encrypted NUS dump into the `code/` `content/` `meta/` tree Cemu
//! expects.
//!
//! This is a port of cdecrypt (VitaSmith / crediar); the reference C sources
//! are kept in `reference/` so the two can be diffed when something drifts.

use std::{
    fs::{self, File},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use aes::Aes128;
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::NoPadding};
use anyhow::{Context, Result, anyhow, bail, ensure};
use indicatif::ProgressBar;
use rayon::prelude::*;
use sha1::{Digest, Sha1};

use crate::fst::{self, FstFile, KeyKind, Tmd};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

const WIIU_COMMON_KEY: [u8; 16] = [
    0xD7, 0xB0, 0x04, 0x02, 0x65, 0x9B, 0xA2, 0xAB, 0xD2, 0xCB, 0x0D, 0xB2, 0x7F, 0xA2, 0xB6, 0x56,
];
const WIIU_COMMON_DEV_KEY: [u8; 16] = [
    0x2F, 0x5C, 0x1B, 0x29, 0x44, 0xE7, 0xFD, 0x6F, 0xC3, 0x97, 0x96, 0x4B, 0x05, 0x76, 0x91, 0xFA,
];

/// Where the wrapped title key sits inside a ticket.
const TICKET_KEY_OFFSET: usize = 0x1BF;

/// Hashed content is stored as 0x400 of hashes followed by 0xFC00 of data.
const HASH_BLOCK: usize = 0xFC00;
const HASHES_SIZE: usize = 0x400;
const HASHED_CHUNK: usize = HASH_BLOCK + HASHES_SIZE; // 0x10000
/// Plain content is just CBC over 0x8000 chunks.
const PLAIN_CHUNK: usize = 0x8000;

pub struct Summary {
    pub files: usize,
    pub bytes: u64,
}

/// Decrypt one CBC buffer in place. Length must be a multiple of 16.
fn cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], buf: &mut [u8]) -> Result<()> {
    let len = buf.len();
    Aes128CbcDec::new(key.into(), iv.into())
        .decrypt_padded::<NoPadding>(buf)
        .map_err(|_| anyhow!("longueur de bloc AES invalide ({len} octets)"))?;
    Ok(())
}

/// Unwrap the title key from the ticket using the appropriate common key.
fn title_key(tmd: &Tmd, ticket: &[u8]) -> Result<[u8; 16]> {
    ensure!(
        ticket.len() >= TICKET_KEY_OFFSET + 16,
        "ticket trop court : {} octets",
        ticket.len()
    );

    let common = match tmd.key_kind {
        KeyKind::Retail => &WIIU_COMMON_KEY,
        KeyKind::Dev => &WIIU_COMMON_DEV_KEY,
    };

    // IV is the title id, zero-padded to a full block.
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&tmd.title_id);

    let mut key = [0u8; 16];
    key.copy_from_slice(&ticket[TICKET_KEY_OFFSET..TICKET_KEY_OFFSET + 16]);
    cbc_decrypt(common, &iv, &mut key)?;
    Ok(key)
}

/// cdecrypt accepts several naming conventions for the content files.
fn content_path(dir: &Path, content_id: u32) -> Result<PathBuf> {
    let candidates = [
        format!("{content_id:08x}.app"),
        format!("{content_id:08X}.app"),
        format!("{content_id:08x}"),
        format!("{content_id:08X}"),
    ];
    for name in &candidates {
        let path = dir.join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("contenu {content_id:08x} introuvable dans {}", dir.display())
}

/// Extract a file from a content that carries an H0 hash tree.
///
/// Port of cdecrypt's `extract_file_hash`.
fn extract_hashed(
    src: &mut File,
    file_offset: u64,
    mut size: u64,
    dst: &Path,
    content_id: u16,
    key: &[u8; 16],
) -> Result<()> {
    let mut out = BufWriter::new(File::create(dst)?);
    let mut enc = vec![0u8; HASHED_CHUNK];

    let mut block_number = (file_offset / HASH_BLOCK as u64) & 0x0F;
    let read_offset = file_offset / HASH_BLOCK as u64 * HASHED_CHUNK as u64;
    let mut skip = (file_offset % HASH_BLOCK as u64) as usize;

    let mut write_size = HASH_BLOCK as u64;
    if skip as u64 + size > write_size {
        write_size -= skip as u64;
    }

    src.seek(SeekFrom::Start(read_offset))?;

    while size > 0 {
        write_size = write_size.min(size);
        src.read_exact(&mut enc)
            .with_context(|| format!("lecture de {} octets pour {}", HASHED_CHUNK, dst.display()))?;

        // The hash block is encrypted with an IV derived from the content id.
        let mut iv = [0u8; 16];
        iv[1] = content_id as u8;
        let mut hashes = [0u8; HASHES_SIZE];
        hashes.copy_from_slice(&enc[..HASHES_SIZE]);
        cbc_decrypt(key, &iv, &mut hashes)?;

        let h0_at = 0x14 * block_number as usize;
        let h0: [u8; 20] = hashes[h0_at..h0_at + 20].try_into().unwrap();

        // The data block's IV is the first 16 bytes of its own H0 hash.
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&hashes[h0_at..h0_at + 16]);
        if block_number == 0 {
            iv[1] ^= content_id as u8;
        }

        let mut dec = enc[HASHES_SIZE..].to_vec();
        cbc_decrypt(key, &iv, &mut dec)?;

        let mut hash: [u8; 20] = Sha1::digest(&dec).into();
        if block_number == 0 {
            hash[1] ^= content_id as u8;
        }
        ensure!(
            hash == h0,
            "hash H0 invalide pour {} (bloc {block_number})",
            dst.display()
        );

        out.write_all(&dec[skip..skip + write_size as usize])?;
        size -= write_size;

        block_number = (block_number + 1) % 16;
        if skip != 0 {
            write_size = HASH_BLOCK as u64;
            skip = 0;
        }
    }

    out.flush()?;
    Ok(())
}

/// Extract a file from a content with no hash tree: plain CBC throughout.
///
/// Port of cdecrypt's `extract_file`.
fn extract_plain(
    src: &mut File,
    file_offset: u64,
    mut size: u64,
    dst: &Path,
    content_id: u16,
    key: &[u8; 16],
) -> Result<()> {
    let mut out = BufWriter::new(File::create(dst)?);
    let mut buf = vec![0u8; PLAIN_CHUNK];

    let read_offset = file_offset / PLAIN_CHUNK as u64 * PLAIN_CHUNK as u64;
    let mut skip = (file_offset % PLAIN_CHUNK as u64) as usize;

    // CBC state runs continuously from the start of the read, seeded from the
    // content id.
    let mut iv = [0u8; 16];
    iv[1] = content_id as u8;

    let mut write_size = PLAIN_CHUNK as u64;
    if skip as u64 + size > write_size {
        write_size -= skip as u64;
    }

    src.seek(SeekFrom::Start(read_offset))?;

    while size > 0 {
        write_size = write_size.min(size);
        src.read_exact(&mut buf)
            .with_context(|| format!("lecture de {PLAIN_CHUNK} octets pour {}", dst.display()))?;

        // Carry the CBC state: the next IV is this chunk's last ciphertext block.
        let next_iv: [u8; 16] = buf[PLAIN_CHUNK - 16..].try_into().unwrap();
        cbc_decrypt(key, &iv, &mut buf)?;
        iv = next_iv;

        out.write_all(&buf[skip..skip + write_size as usize])?;
        size -= write_size;

        if skip != 0 {
            write_size = PLAIN_CHUNK as u64;
            skip = 0;
        }
    }

    out.flush()?;
    Ok(())
}

/// Decrypt a downloaded NUS dump into `out_dir`.
///
/// `dump_dir` must hold `title.tmd`, `title.tik` and the `.app` contents.
pub fn decrypt_dump(dump_dir: &Path, out_dir: &Path, progress: Option<&ProgressBar>) -> Result<Summary> {
    let tmd_bytes = fs::read(dump_dir.join("title.tmd"))
        .with_context(|| format!("title.tmd introuvable dans {}", dump_dir.display()))?;
    let ticket = fs::read(dump_dir.join("title.tik"))
        .with_context(|| format!("title.tik introuvable dans {}", dump_dir.display()))?;

    let tmd = Tmd::parse(&tmd_bytes)?;
    let key = title_key(&tmd, &ticket)?;

    let first = *tmd
        .contents
        .first()
        .ok_or_else(|| anyhow!("le TMD ne déclare aucun contenu"))?;

    // Content 0 holds the FST; it is decrypted whole, with a zero IV.
    let fst_path = content_path(dump_dir, first.id)?;
    let mut fst_data = fs::read(&fst_path)?;
    ensure!(
        fst_data.len() as u64 == first.size,
        "taille du contenu {:08x} incorrecte : {} au lieu de {}",
        first.id,
        fst_data.len(),
        first.size
    );
    cbc_decrypt(&key, &[0u8; 16], &mut fst_data)?;

    let files = fst::parse(&fst_data).context(
        "impossible de lire la FST — la clé de titre est probablement incorrecte",
    )?;

    let total: u64 = files.iter().map(|f| f.length).sum();
    if let Some(bar) = progress {
        bar.set_length(total);
        bar.set_position(0);
    }

    // Create every directory up front so the extraction itself can run in
    // parallel without racing on directory creation.
    fs::create_dir_all(out_dir)?;
    for file in &files {
        if let Some(parent) = Path::new(&file.path).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(out_dir.join(parent))?;
            }
        }
    }

    // Resolve each content's `.app` path once, up front, so a missing file is
    // reported before any extraction starts.
    let content_paths = tmd
        .contents
        .iter()
        .map(|c| content_path(dump_dir, c.id))
        .collect::<Result<Vec<_>>>()?;

    files.par_iter().try_for_each(|file: &FstFile| -> Result<()> {
        let content = *tmd
            .contents
            .get(file.content_index)
            .ok_or_else(|| anyhow!("index de contenu {} hors limites", file.content_index))?;
        let dst = out_dir.join(&file.path);
        let mut src = File::open(&content_paths[file.content_index])?;

        if content.is_hashed() {
            extract_hashed(&mut src, file.offset, file.length, &dst, content.index, &key)
        } else {
            extract_plain(&mut src, file.offset, file.length, &dst, content.index, &key)
        }
        .with_context(|| format!("extraction de {}", file.path))?;

        if let Some(bar) = progress {
            bar.inc(file.length);
        }
        Ok(())
    })?;

    Ok(Summary {
        files: files.len(),
        bytes: total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WW_DUMP: &str =
        "/Users/alexfougeroux/Documents/Cemu/FunKiiU/WindWakerHD/install/0005000010143500";

    #[test]
    fn unwraps_the_wind_waker_title_key() {
        let Ok(tmd_bytes) = fs::read(Path::new(WW_DUMP).join("title.tmd")) else {
            return; // dump absent
        };
        let ticket = fs::read(Path::new(WW_DUMP).join("title.tik")).unwrap();
        let tmd = Tmd::parse(&tmd_bytes).unwrap();

        assert_eq!(tmd.key_kind, KeyKind::Retail);
        assert_eq!(hex::encode(tmd.title_id), "0005000010143500");
        // A wrong key would not produce a valid FST, which the full test covers.
        assert_eq!(title_key(&tmd, &ticket).unwrap().len(), 16);
    }

    #[test]
    fn fst_matches_the_known_dump() {
        let Ok(tmd_bytes) = fs::read(Path::new(WW_DUMP).join("title.tmd")) else {
            return;
        };
        let ticket = fs::read(Path::new(WW_DUMP).join("title.tik")).unwrap();
        let tmd = Tmd::parse(&tmd_bytes).unwrap();
        let key = title_key(&tmd, &ticket).unwrap();

        let mut data = fs::read(content_path(Path::new(WW_DUMP), tmd.contents[0].id).unwrap()).unwrap();
        cbc_decrypt(&key, &[0u8; 16], &mut data).unwrap();
        let files = fst::parse(&data).unwrap();

        assert!(files.iter().any(|f| f.path == "code/cking.rpx"));
        assert!(files.iter().any(|f| f.path == "meta/meta.xml"));
        assert!(files.iter().any(|f| f.path.starts_with("content/")));
    }
}
