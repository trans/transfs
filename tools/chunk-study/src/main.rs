//! Chunk-sharing measurement (whitepaper §6, gate 2).
//!
//! Read-only: it reads the files named on the command line and prints a
//! Markdown table, one row per chunking method. Two modes:
//!
//! - `corpus LABEL FILE...` puts every file into one store and reports what
//!   the whole set costs to keep.
//! - `pairs LABEL OLD NEW [OLD NEW ...]` gives each pair a fresh store holding
//!   OLD, then reports what NEW adds. Rows are summed over the pairs.
//! - `zpairs LABEL OLD NEW [OLD NEW ...]` is `pairs` with each stored chunk
//!   compressed by zstd (see `compress.rs`).
//!
//! Whole-file storage is the baseline: one object per distinct file, as
//! transfs stores blobs today. Chunked methods also pay for a pack index entry
//! per new chunk and a manifest entry per chunk reference, and are grouped
//! into 4 MiB packs.
use sha2::{Digest, Sha256};
use std::{collections::HashSet, env, fs, process, time::Instant};

mod compress;

const PACK_TARGET: u64 = 4 << 20;
/// A pack index entry: 32-byte identity, 8-byte offset, 8-byte length.
const INDEX_ENTRY: u64 = 48;
/// One chunk hash in a file version's chunk list.
const MANIFEST_ENTRY: u64 = 32;

#[derive(Clone, Copy)]
enum Method {
    Whole,
    Fixed(usize),
    Cdc(u32),
}

const METHODS: [Method; 10] = [
    Method::Whole,
    Method::Fixed(1 << 10),
    Method::Fixed(4 << 10),
    Method::Fixed(16 << 10),
    Method::Fixed(64 << 10),
    Method::Cdc(4 << 10),
    Method::Cdc(8 << 10),
    Method::Cdc(16 << 10),
    Method::Cdc(32 << 10),
    Method::Cdc(64 << 10),
];

impl Method {
    fn name(self) -> String {
        match self {
            Self::Whole => "whole file".into(),
            Self::Fixed(size) => format!("fixed {} KiB", size >> 10),
            Self::Cdc(avg) => format!("FastCDC avg {} KiB", avg >> 10),
        }
    }

    fn chunks(self, data: &[u8]) -> Vec<&[u8]> {
        match self {
            Self::Whole => vec![data],
            Self::Fixed(size) => data.chunks(size).collect(),
            // FastCDC 2020 with the usual normalized bounds: min avg/4, max avg*4.
            Self::Cdc(avg) => fastcdc::v2020::FastCDC::new(data, avg / 4, avg, avg * 4)
                .map(|chunk| &data[chunk.offset..chunk.offset + chunk.length])
                .collect(),
        }
    }
}

#[derive(Default)]
struct Tally {
    input: u64,
    stored: u64,
    overhead: u64,
    new_chunks: u64,
    refs: u64,
    objects: u64,
    hashed: u64,
    secs: f64,
}

/// Chunks and whole files already stored. A file version's chunk list is
/// content-addressed too, so a file the store already holds adds nothing.
#[derive(Default)]
struct Store {
    chunks: HashSet<[u8; 32]>,
    files: HashSet<[u8; 32]>,
}

/// What one file adds to a store.
struct Added {
    bytes: u64,
    chunks: u64,
    refs: u64,
}

fn add(store: &mut Store, method: Method, data: &[u8], tally: &mut Tally) -> Added {
    let start = Instant::now();
    if !store.files.insert(Sha256::digest(data).into()) {
        tally.secs += start.elapsed().as_secs_f64();
        tally.hashed += data.len() as u64;
        return Added {
            bytes: 0,
            chunks: 0,
            refs: 0,
        };
    }
    let chunks = method.chunks(data);
    let mut added = Added {
        bytes: 0,
        chunks: 0,
        refs: chunks.len() as u64,
    };
    for chunk in chunks {
        if store.chunks.insert(Sha256::digest(chunk).into()) {
            added.bytes += chunk.len() as u64;
            added.chunks += 1;
        }
    }
    tally.secs += start.elapsed().as_secs_f64();
    tally.hashed += data.len() as u64;
    added
}

/// Charge one stored version to the tally.
fn charge(method: Method, added: &Added, input: u64, tally: &mut Tally) {
    tally.input += input;
    tally.stored += added.bytes;
    tally.new_chunks += added.chunks;
    tally.refs += added.refs;
    if let Method::Whole = method {
        tally.objects += added.chunks; // one blob object per new distinct file
    } else {
        let overhead = added.chunks * INDEX_ENTRY + added.refs * MANIFEST_ENTRY;
        tally.overhead += overhead;
        tally.objects += (added.bytes + overhead).div_ceil(PACK_TARGET);
    }
}

fn read(path: &str) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        process::exit(1);
    })
}

fn mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1 << 20) as f64)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let (mode, label, files) = match args.as_slice() {
        [mode, label, files @ ..] if !files.is_empty() => (mode.as_str(), label, files),
        _ => {
            eprintln!("usage: chunk-study corpus LABEL FILE...\n       chunk-study pairs|zpairs LABEL OLD NEW [OLD NEW ...]");
            process::exit(2);
        }
    };
    if mode != "corpus" && files.len() % 2 != 0 {
        eprintln!("{mode} needs an even number of files");
        process::exit(2);
    }
    if mode == "zpairs" {
        compress::zpairs(label, files);
        return;
    }
    if mode != "corpus" && mode != "pairs" {
        eprintln!("unknown mode {mode}");
        process::exit(2);
    }

    let mut tallies: Vec<Tally> = METHODS.iter().map(|_| Tally::default()).collect();
    if mode == "corpus" {
        let mut stores: Vec<Store> = METHODS.iter().map(|_| Store::default()).collect();
        for path in files {
            let data = read(path);
            for (i, method) in METHODS.iter().enumerate() {
                let added = add(&mut stores[i], *method, &data, &mut tallies[i]);
                charge(*method, &added, data.len() as u64, &mut tallies[i]);
            }
        }
    } else {
        for pair in files.chunks(2) {
            let (old, new) = (read(&pair[0]), read(&pair[1]));
            for (i, method) in METHODS.iter().enumerate() {
                let mut store = Store::default();
                let mut scratch = Tally::default();
                add(&mut store, *method, &old, &mut tallies[i]);
                let added = add(&mut store, *method, &new, &mut scratch);
                tallies[i].secs += scratch.secs;
                tallies[i].hashed += scratch.hashed;
                charge(*method, &added, new.len() as u64, &mut tallies[i]);
            }
        }
    }

    let count = if mode == "pairs" {
        files.len() / 2
    } else {
        files.len()
    };
    let unit = if mode == "pairs" { "pairs" } else { "files" };
    let baseline = tallies[0].stored + tallies[0].overhead;
    println!("### {label}\n");
    println!(
        "{count} {unit}, {} MiB input{}.\n",
        mib(tallies[0].input),
        if mode == "pairs" {
            " (new versions only)"
        } else {
            ""
        }
    );
    println!("| method | stored MiB | overhead MiB | total MiB | vs whole file | new chunks | avg chunk | objects | MB/s |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for (method, t) in METHODS.iter().zip(&tallies) {
        let total = t.stored + t.overhead;
        let avg = if t.refs == 0 { 0 } else { t.input / t.refs };
        let speed = if t.secs > 0.0 {
            t.hashed as f64 / t.secs / 1e6
        } else {
            0.0
        };
        let ratio = if baseline == 0 {
            "–".to_string()
        } else {
            format!("{:.1}%", 100.0 * total as f64 / baseline as f64)
        };
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {:.0} |",
            method.name(),
            mib(t.stored),
            mib(t.overhead),
            mib(total),
            ratio,
            t.new_chunks,
            avg,
            t.objects,
            speed
        );
    }
    println!();
}
