# wuget

Download and decrypt Wii U content in a single command. It merges what
FunKiiU (CDN protocol, tickets) and cdecrypt (AES decryption, FST extraction)
used to do separately into one Rust binary, no Python or external tools
required.

```
Title ID ──▶ ticket ──▶ Nintendo CDN ──▶ decryption ──▶ code/ content/ meta/
```

## Installation

Download the binary for your OS from the [Releases](../../releases) page,
or build it yourself:

```sh
cargo install --path .
```

## Usage

```sh
wuget                                # interactive picker (3621 titles)
wuget get 0005000010143500           # direct download, by Title ID
wuget search zelda --region EUR      # search the catalog
wuget decrypt <dump>                 # decrypt an existing NUS dump
wuget ticket <id> [--generated]      # print a ticket to stdout
```

Global options: `-o/--output` (default `~/Documents/Cemu/games`), `--keep`
(keep the `.app`/`.h3` files), `--no-decrypt`, `--jobs N` (concurrent
downloads, default 3), `--retry N`, `--no-patch-dlc`, `--no-patch-demo`.

In the picker: type to filter, `Tab` switches region, `Shift+Tab` switches
type, `Space` toggles multi-selection (when the search is empty), `Enter`
launches.

## Tickets

Three sources, in order of preference:

1. **Nintendo cetk** for updates — legitimate;
2. **embedded legitimate ticket** (964 titles) — installs without a
   signature patch on real hardware;
3. **generated ticket** derived from the catalog key — works in Cemu, but
   requires signature patches on real hardware.

The source used is displayed for every download.

## Output

`<output>/<Name> [REGION]/{code,content,meta}`, directly loadable in Cemu via
*File ▸ Load* on the `.rpx` in `code/`. Intermediate encrypted files are
removed after a successful decryption (`--keep` to keep them); on failure
they are always kept, so you don't have to re-download.

## Verification

The port is validated against the original tools:

- the ticket produced by `wuget ticket <id> --generated` is byte-for-byte
  identical to FunKiiU's;
- the output of `wuget decrypt` is identical to cdecrypt's (silent `diff -r`
  on 1018 files / 1.7 GB of *The Wind Waker HD*);
- every hashed content block has its SHA-1 H0 verified during extraction, so
  a wrong key fails loudly instead of writing corrupted data.

`cargo test` covers catalog parsing, both ticket paths, the FST, and the
picker.

## Embedded data

`data/` contains the key database (3621 titles), the 964 legitimate tickets,
the common certificate, the ticket template, and the DLC unlock patch. All
of it is compiled into the binary by `build.rs`, which packs the tickets
into a single indexed blob.

`reference/` keeps the original cdecrypt C sources used for the port.

## License and attribution

GPL-3.0-or-later, inherited from cdecrypt, of which `src/decrypt.rs` and
`src/fst.rs` are a port.

- **cdecrypt** — © 2020-2023 VitaSmith, © 2013-2015 crediar, GPL-3.0.
  <https://github.com/VitaSmith/cdecrypt>
- **FunKiiU** — cearp and the cerea1killer; `src/ticket.rs` and
  `src/download.rs` port its CDN protocol and ticket fabrication.

The contents of `data/` (key database, tickets) come from a public mirror
of the Wii U Title Key Database and are not covered by this license.
