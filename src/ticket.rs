//! Ticket sourcing.
//!
//! A ticket carries the (wrapped) title key. Three ways to get one, in
//! decreasing order of desirability — a legit ticket installs on real hardware
//! without signature patches, a generated one does not.

use anyhow::Result;

use crate::catalog::{self, Kind};

/// The signed certificate chain, identical for every title.
pub const TITLE_CERT: &[u8] = include_bytes!("../data/title.cert");

/// Ticket skeleton lifted from FunKiiU, with placeholder key/id/version.
const TEMPLATE: &[u8] = include_bytes!("../data/ticket_template.tik");

/// Overwrites the DLC index table so every piece of DLC reads as owned.
const DLC_UNLOCK: &[u8] = include_bytes!("../data/dlc_unlock.bin");

/// All ticket fields are relative to the end of the signature block.
const TK: usize = 0x140;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Nintendo's own cetk, fetched from the CDN (updates only).
    NintendoCetk,
    /// A real ticket from the bundled mirror.
    BundledLegit,
    /// Forged locally from a title key.
    Generated,
}

impl Source {
    pub fn describe(self) -> &'static str {
        match self {
            Source::NintendoCetk => "ticket légitime (CDN Nintendo)",
            Source::BundledLegit => "ticket légitime (base embarquée)",
            Source::Generated => "ticket généré (nécessite des patchs de signature sur console)",
        }
    }

    /// Whether the resulting install works on unmodified hardware.
    pub fn is_legit(self) -> bool {
        self != Source::Generated
    }
}

/// Build a ticket from a title key, patching DLC/demo restrictions away.
///
/// Port of FunKiiU's `make_ticket`.
pub fn generate(
    title_id: &str,
    title_key: [u8; 16],
    title_version: [u8; 2],
    patch_dlc: bool,
    patch_demo: bool,
) -> Result<Vec<u8>> {
    let mut tik = TEMPLATE.to_vec();
    let id = hex::decode(title_id)?;

    tik[TK + 0x9C..TK + 0xA4].copy_from_slice(&id);
    tik[TK + 0xA6..TK + 0xA8].copy_from_slice(&title_version);
    tik[TK + 0x7F..TK + 0x8F].copy_from_slice(&title_key);

    match catalog::Kind::from_title_id(title_id) {
        Kind::Demo if patch_demo => {
            // Zeroing the play-count block removes the launch limit.
            tik[TK + 0x124..TK + 0x164].fill(0);
        }
        Kind::Dlc if patch_dlc => {
            tik[TK + 0x164..TK + 0x210].copy_from_slice(DLC_UNLOCK);
        }
        _ => {}
    }

    Ok(tik)
}

/// The legit ticket bundled for this title, if any.
pub fn bundled(title_id: &str) -> Option<&'static [u8]> {
    catalog::bundled_ticket(title_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ticket we generate must be byte-identical to the one FunKiiU
    /// produced for the Wind Waker HD dump sitting next door.
    #[test]
    fn matches_funkiiu_output() {
        let reference = match std::fs::read(
            "/Users/alexfougeroux/Documents/Cemu/FunKiiU/WindWakerHD/install/0005000010143500/title.tik",
        ) {
            Ok(b) => b,
            Err(_) => return, // dump absent, rien à comparer
        };

        let key = catalog::check_title_key("3cd545e19bbcb54e41db3169f7432ea1").unwrap();
        // Title version comes from the TMD of that same dump.
        let tmd = std::fs::read(
            "/Users/alexfougeroux/Documents/Cemu/FunKiiU/WindWakerHD/install/0005000010143500/title.tmd",
        )
        .unwrap();
        let tmd = crate::fst::Tmd::parse(&tmd).unwrap();

        let ours = generate("0005000010143500", key, tmd.title_version, true, true).unwrap();
        assert_eq!(ours.len(), reference.len());
        assert_eq!(ours, reference, "ticket généré différent de celui de FunKiiU");
    }

    #[test]
    fn cert_is_the_expected_blob() {
        assert_eq!(TITLE_CERT.len(), 2560);
    }
}
