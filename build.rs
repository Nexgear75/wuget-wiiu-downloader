//! Packs the bundled legit tickets into a single blob plus a sorted index,
//! so the binary stays self-contained without 964 separate `include_bytes!`.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=data/tickets");

    let mut tickets: Vec<(String, Vec<u8>)> = fs::read_dir("data/tickets")
        .expect("data/tickets is missing")
        .filter_map(|e| {
            let path = e.ok()?.path();
            if path.extension()? != "tik" {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_ascii_lowercase();
            Some((id, fs::read(&path).ok()?))
        })
        .collect();

    // Sorted so the lookup can binary search.
    tickets.sort_by(|a, b| a.0.cmp(&b.0));

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut blob = Vec::new();
    let mut index = String::from(
        "/// (title id, offset into TICKET_BLOB, length), sorted by title id.\n\
         pub static TICKET_INDEX: &[(&str, u32, u32)] = &[\n",
    );

    for (id, data) in &tickets {
        index.push_str(&format!(
            "    (\"{}\", {}, {}),\n",
            id,
            blob.len(),
            data.len()
        ));
        blob.extend_from_slice(data);
    }
    index.push_str("];\n");

    fs::write(out.join("tickets.bin"), &blob).unwrap();
    fs::write(out.join("ticket_index.rs"), index).unwrap();
}
