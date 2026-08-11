//! wuget — télécharge et déchiffre du contenu Wii U en une seule commande.
//!
//! Réunit le rôle de FunKiiU (protocole CDN, tickets) et celui de cdecrypt
//! (déchiffrement AES, extraction FST) dans un seul binaire autonome.

mod catalog;
mod decrypt;
mod download;
mod fst;
mod picker;
mod ticket;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use catalog::{Kind, Title};

#[derive(Parser)]
#[command(
    name = "wuget",
    version,
    about = "Télécharge et déchiffre du contenu Wii U, prêt pour Cemu",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Dossier de destination des jeux déchiffrés.
    #[arg(short, long, global = true)]
    output: Option<PathBuf>,

    /// Conserve les fichiers chiffrés (.app/.h3) après déchiffrement.
    #[arg(long, global = true)]
    keep: bool,

    /// S'arrête après le téléchargement, sans déchiffrer.
    #[arg(long, global = true)]
    no_decrypt: bool,

    /// Téléchargements simultanés.
    #[arg(long, default_value_t = 3, global = true)]
    jobs: usize,

    /// Tentatives par fichier avant abandon.
    #[arg(long, default_value_t = 4, global = true)]
    retry: u32,

    /// Ne pas déverrouiller tous les DLC dans le ticket généré.
    #[arg(long, global = true)]
    no_patch_dlc: bool,

    /// Ne pas retirer la limite de parties des démos.
    #[arg(long, global = true)]
    no_patch_demo: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Télécharge et déchiffre un ou plusieurs Title ID.
    Get {
        /// Title ID sur 16 caractères hexadécimaux.
        title_ids: Vec<String>,

        /// Clé de titre (32 hex) pour un titre absent du catalogue.
        #[arg(long)]
        key: Option<String>,
    },

    /// Cherche dans le catalogue embarqué.
    Search {
        query: Vec<String>,

        /// Limite les résultats à une région (EUR, USA, JPN…).
        #[arg(long)]
        region: Option<String>,
    },

    /// Déchiffre un dump NUS déjà présent sur le disque.
    Decrypt {
        /// Dossier contenant title.tmd, title.tik et les .app.
        dump_dir: PathBuf,
    },

    /// Écrit sur la sortie standard le ticket qui serait utilisé.
    Ticket {
        title_id: String,

        #[arg(long)]
        key: Option<String>,

        /// Force un ticket généré depuis la clé, même si un ticket légitime existe.
        #[arg(long)]
        generated: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output = cli
        .output
        .clone()
        .unwrap_or_else(|| default_output_dir());

    match &cli.command {
        None => {
            let chosen = picker::run()?;
            if chosen.is_empty() {
                println!("Rien à faire.");
                return Ok(());
            }
            run_pipeline(&cli, &output, &chosen, None)
        }

        Some(Command::Get { title_ids, key }) => {
            if title_ids.is_empty() {
                bail!("donne au moins un Title ID, ou lance `wuget` sans argument");
            }
            let key = key
                .as_deref()
                .map(catalog::check_title_key)
                .transpose()?;

            let mut titles = Vec::new();
            let mut owned = Vec::new();
            for id in title_ids {
                let id = catalog::check_title_id(id)?;
                match catalog::find(&id) {
                    Some(t) => titles.push(t),
                    None if key.is_some() => owned.push(synthetic_title(&id)),
                    None => bail!(
                        "{id} est absent du catalogue — fournis sa clé avec --key"
                    ),
                }
            }
            // Titles built on the fly need to outlive the loop above.
            let owned: Vec<&Title> = owned.iter().collect();
            run_pipeline(&cli, &output, &titles, key)?;
            run_pipeline(&cli, &output, &owned, key)
        }

        Some(Command::Search { query, region }) => {
            search(&query.join(" "), region.as_deref());
            Ok(())
        }

        Some(Command::Decrypt { dump_dir }) => {
            let name = dump_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "dump".into());
            let dst = output.join(name);
            let bar = decrypt_bar();
            let summary = decrypt::decrypt_dump(dump_dir, &dst, Some(&bar))?;
            bar.finish_and_clear();
            println!(
                "{} fichiers ({}) → {}",
                summary.files,
                human(summary.bytes),
                dst.display()
            );
            Ok(())
        }

        Some(Command::Ticket {
            title_id,
            key,
            generated,
        }) => {
            let id = catalog::check_title_id(title_id)?;
            let tik = ticket_for(&id, key.as_deref(), &cli, *generated)?;
            std::io::stdout().write_all(&tik)?;
            Ok(())
        }
    }
}

fn default_output_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Documents/Cemu/games")
}

/// A title we know nothing about beyond its id — the user supplied the key.
fn synthetic_title(title_id: &str) -> Title {
    Title {
        kind: Kind::from_title_id(title_id),
        title_id: title_id.to_string(),
        title_key: None,
        name: title_id.to_string(),
        region: "???".into(),
        has_ticket: false,
    }
}

fn decrypt_bar() -> ProgressBar {
    let bar = ProgressBar::new(0);
    bar.set_style(
        ProgressStyle::with_template(
            "  Déchif [{bar:34.magenta/dim}] {bytes}/{total_bytes}  ETA {eta}",
        )
        .unwrap()
        .progress_chars("━━╸"),
    );
    bar
}

fn search(query: &str, region: Option<&str>) {
    let needle = query.to_lowercase();
    let mut hits: Vec<&Title> = catalog::all()
        .iter()
        .filter(|t| t.kind.is_user_facing())
        .filter(|t| region.is_none_or(|r| t.region.eq_ignore_ascii_case(r)))
        .filter(|t| {
            needle.is_empty()
                || t.name.to_lowercase().contains(&needle)
                || t.title_id.contains(&needle)
        })
        .collect();
    hits.sort_by(|a, b| a.name.cmp(&b.name).then(a.region.cmp(&b.region)));

    if hits.is_empty() {
        println!("Aucun résultat.");
        return;
    }
    for t in &hits {
        println!(
            "{}  {:<4}  {:<7}  {:<6}  {}",
            t.title_id,
            t.region,
            t.kind.label(),
            if t.has_ticket { "légit" } else { "-" },
            t.name
        );
    }
    println!("\n{} résultat(s).", hits.len());
}

fn ticket_for(
    title_id: &str,
    key: Option<&str>,
    cli: &Cli,
    force_generated: bool,
) -> Result<Vec<u8>> {
    if !force_generated {
        if let Some(tik) = ticket::bundled(title_id) {
            return Ok(tik.to_vec());
        }
    }
    let key = key
        .map(catalog::check_title_key)
        .transpose()?
        .or_else(|| {
            catalog::find(title_id)
                .and_then(|t| t.title_key.as_deref())
                .and_then(|k| catalog::check_title_key(k).ok())
        })
        .context("aucune clé connue pour ce titre — utilise --key")?;

    // The title version lives in the TMD, which we would have to fetch; for a
    // standalone ticket, zero is what FunKiiU would have written before its
    // own TMD download, so fetch it to stay faithful.
    let tmd_version = fetch_title_version(title_id)?;
    ticket::generate(
        title_id,
        key,
        tmd_version,
        !cli.no_patch_dlc,
        !cli.no_patch_demo,
    )
}

/// Grab just the title version from the CDN's TMD.
fn fetch_title_version(title_id: &str) -> Result<[u8; 2]> {
    let url = format!(
        "http://ccs.cdn.c.shop.nintendowifi.net/ccs/download/{title_id}/tmd"
    );
    let mut response = ureq::get(&url).call()?;
    let bytes = response.body_mut().read_to_vec()?;
    Ok(fst::Tmd::parse(&bytes)?.title_version)
}

fn run_pipeline(
    cli: &Cli,
    output: &Path,
    titles: &[&Title],
    key: Option<[u8; 16]>,
) -> Result<()> {
    if titles.is_empty() {
        return Ok(());
    }

    let options = download::Options {
        retry: cli.retry,
        jobs: cli.jobs,
        patch_dlc: !cli.no_patch_dlc,
        patch_demo: !cli.no_patch_demo,
    };

    for (i, title) in titles.iter().enumerate() {
        println!(
            "\n[{}/{}] {}  ({}, {})",
            i + 1,
            titles.len(),
            title.name,
            title.region,
            title.kind.label()
        );

        if let Err(e) = one_title(cli, output, title, key, &options) {
            eprintln!("  ✗ {title_id} : {e:#}", title_id = title.title_id);
        }
    }
    Ok(())
}

fn one_title(
    cli: &Cli,
    output: &Path,
    title: &Title,
    key: Option<[u8; 16]>,
    options: &download::Options,
) -> Result<()> {
    let started = Instant::now();
    let name = title.output_dir_name();
    let encrypted_dir = output.join(format!("{name}.nus"));
    let decrypted_dir = output.join(&name);

    let multi = MultiProgress::new();
    let downloaded = download::download_title(title, key, &encrypted_dir, options, &multi)?;

    if cli.no_decrypt {
        println!(
            "  ✓ Dump chiffré ({}) → {}",
            human(downloaded.tmd.total_size()),
            encrypted_dir.display()
        );
        return Ok(());
    }

    let bar = multi.add(decrypt_bar());
    let summary = decrypt::decrypt_dump(&encrypted_dir, &decrypted_dir, Some(&bar))?;
    bar.finish_and_clear();
    drop(multi);

    // Only now is it safe to drop the encrypted copy.
    if !cli.keep {
        fs::remove_dir_all(&encrypted_dir)
            .with_context(|| format!("suppression de {}", encrypted_dir.display()))?;
    }

    println!(
        "  ✓ {} fichiers ({}) en {:.0} s → {}",
        summary.files,
        human(summary.bytes),
        started.elapsed().as_secs_f64(),
        decrypted_dir.display()
    );
    if !downloaded.ticket_source.is_legit() {
        println!("    ticket généré : installation sur console réelle = patchs de signature requis");
    }
    let rpx = decrypted_dir.join("code");
    println!("    Cemu → File ▸ Load ▸ {}", rpx.display());

    Ok(())
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["o", "Ko", "Mo", "Go", "To"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} o")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}
