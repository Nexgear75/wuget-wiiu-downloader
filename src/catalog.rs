//! The bundled Wii U title key database.
//!
//! Both the key list and the legit tickets are compiled into the binary, so
//! `wuget` needs no network access and no configuration to browse the catalog.

use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

static TICKET_BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tickets.bin"));
include!(concat!(env!("OUT_DIR"), "/ticket_index.rs"));

const TITLEKEYS_JSON: &str = include_str!("../data/titlekeys.json");

/// What a title id's type field (`title_id[4..8]`) says the title is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Game,
    Dlc,
    Update,
    Demo,
    System,
}

impl Kind {
    pub fn from_title_id(title_id: &str) -> Self {
        match &title_id[4..8] {
            "0000" => Kind::Game,
            "000c" => Kind::Dlc,
            "000e" => Kind::Update,
            "0002" => Kind::Demo,
            _ => Kind::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Game => "Jeu",
            Kind::Dlc => "DLC",
            Kind::Update => "Update",
            Kind::Demo => "Démo",
            Kind::System => "Système",
        }
    }

    /// Titles worth offering in the picker. System titles are noise.
    pub fn is_user_facing(self) -> bool {
        self != Kind::System
    }
}

/// One row of the bundled database, as stored on disk.
#[derive(Debug, Deserialize)]
struct RawTitle {
    #[serde(rename = "titleID")]
    title_id: String,
    #[serde(rename = "titleKey")]
    title_key: Option<String>,
    name: Option<String>,
    region: Option<String>,
    ticket: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Title {
    pub title_id: String,
    /// Absent for a handful of ticket-only entries.
    pub title_key: Option<String>,
    /// Newlines in the source data are already flattened to " — ".
    pub name: String,
    pub region: String,
    /// The database claims a legit ticket exists for this title.
    pub has_ticket: bool,
    pub kind: Kind,
}

impl Title {
    /// A title is only downloadable if we can produce a ticket for it: either a
    /// key to generate one from, a bundled legit ticket, or — for updates —
    /// Nintendo's own cetk.
    pub fn is_obtainable(&self) -> bool {
        self.title_key.is_some() || bundled_ticket(&self.title_id).is_some() || self.kind == Kind::Update
    }

    /// Directory name for the decrypted output, e.g. `Pikmin 3 [EUR]`.
    pub fn output_dir_name(&self) -> String {
        let base = safe_filename(&self.name.replace(" — ", " "));
        let suffix = match self.kind {
            Kind::Dlc => " - DLC",
            Kind::Update => " - Update",
            Kind::Demo => " - Demo",
            _ => "",
        };
        format!("{base} [{}]{suffix}", self.region)
    }
}

fn catalog() -> &'static [Title] {
    static CATALOG: OnceLock<Vec<Title>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let raw: Vec<RawTitle> =
            serde_json::from_str(TITLEKEYS_JSON).expect("bundled titlekeys.json is malformed");
        raw.into_iter()
            .filter(|t| t.title_id.len() == 16)
            .map(|t| {
                let title_id = t.title_id.to_ascii_lowercase();
                Title {
                    kind: Kind::from_title_id(&title_id),
                    // Names in the database wrap onto several lines.
                    name: t.name.map(|n| join_name_lines(&n))
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| format!("(sans nom) {title_id}")),
                    region: t.region.unwrap_or_else(|| "???".into()),
                    has_ticket: t.ticket.as_deref() == Some("1"),
                    title_key: t.title_key.filter(|k| k.len() == 32),
                    title_id,
                }
            })
            .collect()
    })
}

/// Every entry in the database, including system titles.
pub fn all() -> &'static [Title] {
    catalog()
}

/// The entries the picker shows: real content, and only what we can obtain.
pub fn browsable() -> Vec<&'static Title> {
    catalog()
        .iter()
        .filter(|t| t.kind.is_user_facing())
        .collect()
}

pub fn find(title_id: &str) -> Option<&'static Title> {
    let needle = title_id.to_ascii_lowercase();
    catalog().iter().find(|t| t.title_id == needle)
}

/// The legit ticket bundled for this title, if the mirror had one.
pub fn bundled_ticket(title_id: &str) -> Option<&'static [u8]> {
    let needle = title_id.to_ascii_lowercase();
    let idx = TICKET_INDEX
        .binary_search_by(|(id, _, _)| (*id).cmp(needle.as_str()))
        .ok()?;
    let (_, offset, len) = TICKET_INDEX[idx];
    Some(&TICKET_BLOB[offset as usize..(offset + len) as usize])
}

/// Distinct regions present in the database, in a stable order.
pub fn regions() -> Vec<&'static str> {
    let mut seen: Vec<&str> = Vec::new();
    for t in catalog() {
        if !seen.contains(&t.region.as_str()) {
            seen.push(&t.region);
        }
    }
    seen.sort_unstable();
    seen
}

pub fn check_title_id(title_id: &str) -> Result<String> {
    let id = title_id.trim().to_ascii_lowercase();
    if id.len() != 16 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("un Title ID fait 16 caractères hexadécimaux, reçu : {title_id:?}");
    }
    Ok(id)
}

pub fn check_title_key(title_key: &str) -> Result<[u8; 16]> {
    let key = title_key.trim().to_ascii_lowercase();
    if key.len() != 32 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("une clé de titre fait 32 caractères hexadécimaux, reçu : {title_key:?}");
    }
    let bytes = hex::decode(&key).context("clé de titre invalide")?;
    Ok(bytes.try_into().unwrap())
}

/// Database names wrap across lines. Join them with an em dash, unless the
/// break already sits after punctuation, where a plain space reads better.
fn join_name_lines(name: &str) -> String {
    let mut out = String::new();
    for line in name.split('\n').map(str::trim).filter(|s| !s.is_empty()) {
        if out.is_empty() {
            out.push_str(line);
        } else if out.ends_with([':', '-', ',', '—']) {
            out.push(' ');
            out.push_str(line);
        } else {
            out.push_str(" — ");
            out.push_str(line);
        }
    }
    out
}

/// Strip anything that has no business in a path component.
/// Port of FunKiiU's `safe_filename`.
pub fn safe_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_underscore = false;
    for c in name.chars() {
        let keep = c.is_alphanumeric() || matches!(c, ' ' | '.' | '_');
        let c = if keep { c } else { '_' };
        // Collapse runs of underscores, as the original does with a regex.
        if c == '_' {
            if last_was_underscore {
                continue;
            }
            last_was_underscore = true;
        } else {
            last_was_underscore = false;
        }
        out.push(c);
    }
    out.trim_matches(|c| c == '_' || c == ' ').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_parses() {
        assert!(all().len() > 3000, "catalogue trop petit : {}", all().len());
    }

    #[test]
    fn multiline_names_are_flattened() {
        let ww = find("0005000010143500").expect("Wind Waker HD USA absent");
        assert_eq!(ww.name, "THE LEGEND OF ZELDA — The Wind Waker HD");
        assert_eq!(ww.region, "USA");
        assert_eq!(ww.kind, Kind::Game);
        assert_eq!(
            ww.title_key.as_deref(),
            Some("3cd545e19bbcb54e41db3169f7432ea1")
        );
    }

    #[test]
    fn bundled_tickets_are_reachable() {
        let tik = bundled_ticket("0005000010143500").expect("ticket Wind Waker absent");
        assert!(tik.len() >= 696);
        assert!(bundled_ticket("ffffffffffffffff").is_none());
    }

    #[test]
    fn safe_filename_matches_funkiiu() {
        assert_eq!(safe_filename("Pokémon"), "Pokémon");
        assert_eq!(safe_filename("幻影異聞録♯ＦＥ"), "幻影異聞録_ＦＥ");
        assert_eq!(safe_filename("Batman™: Arkham"), "Batman_ Arkham");
    }

    #[test]
    fn title_id_validation() {
        assert!(check_title_id("0005000010143500").is_ok());
        assert!(check_title_id("00050000101435").is_err());
        assert!(check_title_id("zzzz000010143500").is_err());
    }
}
