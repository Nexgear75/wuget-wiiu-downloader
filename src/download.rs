//! Fetching an encrypted title from Nintendo's content CDN.
//!
//! Port of FunKiiU's `process_title_id`, with parallel content downloads and
//! real progress reporting.

use std::{
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::Path,
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;

use crate::{
    catalog::Title,
    fst::Tmd,
    ticket::{self, Source},
};

const CDN: &str = "http://ccs.cdn.c.shop.nintendowifi.net/ccs/download";

pub struct Options {
    pub retry: u32,
    pub jobs: usize,
    pub patch_dlc: bool,
    pub patch_demo: bool,
}

pub struct Downloaded {
    pub tmd: Tmd,
    pub ticket_source: Source,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into()
}

/// Fetch a URL into memory, retrying on transient failures.
///
/// `Ok(None)` means the server answered 404 and the caller said that is fine.
fn fetch(agent: &ureq::Agent, url: &str, retry: u32, allow_404: bool) -> Result<Option<Vec<u8>>> {
    let mut last: Option<anyhow::Error> = None;

    for attempt in 1..=retry.max(1) {
        match agent.get(url).call() {
            Ok(mut response) => {
                let mut buf = Vec::new();
                match response.body_mut().as_reader().read_to_end(&mut buf) {
                    Ok(_) => return Ok(Some(buf)),
                    Err(e) => last = Some(anyhow!(e)),
                }
            }
            Err(ureq::Error::StatusCode(404)) if allow_404 => return Ok(None),
            Err(e) => last = Some(anyhow!(e)),
        }
        if attempt < retry {
            thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
        }
    }

    Err(last.unwrap_or_else(|| anyhow!("échec"))).with_context(|| format!("téléchargement de {url}"))
}

/// Stream a URL to disk, ticking `bar` as bytes land.
fn fetch_to_file(
    agent: &ureq::Agent,
    url: &str,
    dst: &Path,
    expected: u64,
    retry: u32,
    bar: &ProgressBar,
    total: &ProgressBar,
) -> Result<()> {
    // Already downloaded in a previous run? Don't pay for it twice.
    if let Ok(meta) = fs::metadata(dst) {
        if meta.len() == expected {
            bar.set_position(expected);
            total.inc(expected);
            return Ok(());
        }
    }

    let mut last: Option<anyhow::Error> = None;

    for attempt in 1..=retry.max(1) {
        // A retry restarts the file, so rewind whatever the last attempt counted.
        let carried = bar.position();
        total.set_position(total.position().saturating_sub(carried));
        bar.set_position(0);

        match try_fetch_to_file(agent, url, dst, expected, bar, total) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        if attempt < retry {
            thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
        }
    }

    Err(last.unwrap_or_else(|| anyhow!("échec")))
        .with_context(|| format!("téléchargement de {url}"))
}

fn try_fetch_to_file(
    agent: &ureq::Agent,
    url: &str,
    dst: &Path,
    expected: u64,
    bar: &ProgressBar,
    total: &ProgressBar,
) -> Result<()> {
    let mut response = agent.get(url).call()?;
    let mut reader = response.body_mut().as_reader();
    let mut out = BufWriter::with_capacity(1 << 20, File::create(dst)?);
    let mut buf = vec![0u8; 1 << 16];
    let mut written = 0u64;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        written += n as u64;
        bar.inc(n as u64);
        total.inc(n as u64);
    }
    out.flush()?;

    if expected != 0 && written != expected {
        bail!("taille incorrecte : {written} octets au lieu de {expected}");
    }
    Ok(())
}

/// Download a title's TMD, ticket, certificate and contents into `dir`.
pub fn download_title(
    title: &Title,
    title_key: Option<[u8; 16]>,
    dir: &Path,
    options: &Options,
    multi: &MultiProgress,
) -> Result<Downloaded> {
    fs::create_dir_all(dir)?;
    let agent = agent();
    let base = format!("{CDN}/{}", title.title_id);

    // --- TMD -------------------------------------------------------------
    let spinner = multi.add(ProgressBar::new_spinner());
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message("Métadonnées (TMD)…");

    let tmd_bytes = fetch(&agent, &format!("{base}/tmd"), options.retry, false)
        .context(
            "impossible de récupérer le TMD — vérifie que rien ne bloque \
             l'accès au CDN Nintendo",
        )?
        .ok_or_else(|| anyhow!("TMD absent"))?;
    fs::write(dir.join("title.tmd"), &tmd_bytes)?;
    let tmd = Tmd::parse(&tmd_bytes)?;

    // --- certificate (always the same blob) ------------------------------
    fs::write(dir.join("title.cert"), ticket::TITLE_CERT)?;

    // --- ticket ----------------------------------------------------------
    spinner.set_message("Ticket…");
    let (ticket_bytes, ticket_source) = resolve_ticket(&agent, title, title_key, &tmd, options)?;
    fs::write(dir.join("title.tik"), &ticket_bytes)?;
    spinner.finish_and_clear();

    println!("  Ticket : {}", ticket_source.describe());

    // --- contents --------------------------------------------------------
    let total_size = tmd.total_size();
    let total = multi.add(ProgressBar::new(total_size));
    total.set_style(
        ProgressStyle::with_template(
            "  Total  [{bar:34.green/dim}] {bytes}/{total_bytes}  {binary_bytes_per_sec}  ETA {eta}",
        )?
        .progress_chars("━━╸"),
    );

    let agent = Arc::new(agent);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.jobs.max(1))
        .build()?;

    pool.install(|| -> Result<()> {
        tmd.contents.par_iter().try_for_each(|content| {
            let bar = multi.insert_before(&total, ProgressBar::new(content.size));
            bar.set_style(
                ProgressStyle::with_template(
                    "  {msg:<12} [{bar:34.cyan/dim}] {bytes}/{total_bytes}",
                )
                .unwrap()
                .progress_chars("━━╸"),
            );
            bar.set_message(content.file_name());

            let result = fetch_to_file(
                &agent,
                &format!("{base}/{:08x}", content.id),
                &dir.join(content.file_name()),
                content.size,
                options.retry,
                &bar,
                &total,
            );
            bar.finish_and_clear();
            result?;

            // The .h3 hash file only exists for hashed content; 404 is normal.
            if let Some(h3) = fetch(
                &agent,
                &format!("{base}/{:08x}.h3", content.id),
                options.retry,
                true,
            )? {
                fs::write(dir.join(format!("{:08x}.h3", content.id)), h3)?;
            }
            Ok(())
        })
    })?;

    total.finish_and_clear();

    Ok(Downloaded {
        tmd,
        ticket_source,
    })
}

/// Pick the best available ticket, preferring legit ones.
fn resolve_ticket(
    agent: &ureq::Agent,
    title: &Title,
    title_key: Option<[u8; 16]>,
    tmd: &Tmd,
    options: &Options,
) -> Result<(Vec<u8>, Source)> {
    use crate::catalog::Kind;

    // Updates are signed by Nintendo and their cetk is public.
    if title.kind == Kind::Update {
        let url = format!("{CDN}/{}/cetk", title.title_id);
        if let Some(cetk) = fetch(agent, &url, options.retry, true)? {
            return Ok((cetk, Source::NintendoCetk));
        }
    }

    if let Some(tik) = ticket::bundled(&title.title_id) {
        return Ok((tik.to_vec(), Source::BundledLegit));
    }

    let key = title_key
        .or_else(|| {
            title
                .title_key
                .as_deref()
                .and_then(|k| crate::catalog::check_title_key(k).ok())
        })
        .ok_or_else(|| {
            anyhow!(
                "aucune clé ni ticket disponible pour {} — fournis --key",
                title.title_id
            )
        })?;

    let tik = ticket::generate(
        &title.title_id,
        key,
        tmd.title_version,
        options.patch_dlc,
        options.patch_demo,
    )?;
    Ok((tik, Source::Generated))
}
