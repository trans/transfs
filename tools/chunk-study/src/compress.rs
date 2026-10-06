//! `zpairs`: what a new version costs when each stored chunk is compressed on
//! its own with zstd, plainly and with a dictionary trained on the old version.
//!
//! The whole-file row compresses the file as one stream, which is what
//! compression gets without chunking. A second table stores the new file fresh
//! (every distinct chunk), which shows what compressing small chunks
//! separately loses and how much a dictionary wins back.
use crate::{mib, read, Method, INDEX_ENTRY, MANIFEST_ENTRY};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, time::Instant};
use zstd::bulk::Compressor;

/// zstd level and dictionary size, from `ZSTD_LEVEL` and `ZSTD_DICT_KIB`.
fn level() -> i32 {
    env_or("ZSTD_LEVEL", 3)
}

fn dict_size() -> usize {
    env_or("ZSTD_DICT_KIB", 110) << 10
}

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const ZMETHODS: [Method; 4] = [
    Method::Whole,
    Method::Fixed(4 << 10),
    Method::Cdc(8 << 10),
    Method::Cdc(16 << 10),
];

#[derive(Default)]
struct Cost {
    raw: u64,
    plain: u64,
    dict: u64,
    overhead: u64,
}

#[derive(Default)]
struct ZTally {
    version: Cost,
    fresh: Cost,
    dictionaries: u64,
    zbytes: u64,
    zsecs: f64,
}

/// A dictionary trained on an even sample of the old version's chunks.
fn train(chunks: &[&[u8]]) -> Option<Vec<u8>> {
    if chunks.len() < 16 {
        return None;
    }
    let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
    // About 100 times the dictionary size in samples, as zstd suggests.
    let step = (total / (100 * dict_size())).max(1);
    let samples: Vec<&[u8]> = chunks.iter().step_by(step).copied().collect();
    zstd::dict::from_samples(&samples, dict_size()).ok()
}

/// Stored size of one chunk: compressed, or raw when compression doesn't help.
fn stored(compressor: &mut Compressor, chunk: &[u8]) -> u64 {
    let compressed = compressor.compress(chunk).expect("zstd compresses");
    compressed.len().min(chunk.len()) as u64
}

pub fn zpairs(label: &str, files: &[String]) {
    let mut tallies: Vec<ZTally> = ZMETHODS.iter().map(|_| ZTally::default()).collect();
    let mut input = 0u64;
    for pair in files.chunks(2) {
        let (old, new) = (read(&pair[0]), read(&pair[1]));
        input += new.len() as u64;
        let unchanged = Sha256::digest(&old) == Sha256::digest(&new);
        for (method, t) in ZMETHODS.iter().zip(tallies.iter_mut()) {
            let old_chunks = method.chunks(&old);
            let mut stored_ids: HashSet<[u8; 32]> = old_chunks
                .iter()
                .map(|chunk| Sha256::digest(chunk).into())
                .collect();
            let dictionary = match method {
                Method::Whole => None,
                _ => train(&old_chunks),
            };
            t.dictionaries += dictionary.as_ref().map_or(0, |d| d.len() as u64);
            let mut plain = Compressor::new(level()).expect("zstd level");
            let mut with_dict = dictionary
                .as_ref()
                .map(|d| Compressor::with_dictionary(level(), d).expect("zstd dictionary"));

            let new_chunks = method.chunks(&new);
            let mut fresh_ids = HashSet::new();
            let (mut version_new, mut fresh_new) = (0u64, 0u64);
            for chunk in &new_chunks {
                let id: [u8; 32] = Sha256::digest(chunk).into();
                let first_in_file = fresh_ids.insert(id);
                let new_to_store = !unchanged && stored_ids.insert(id);
                if !first_in_file && !new_to_store {
                    continue;
                }
                let start = Instant::now();
                let p = stored(&mut plain, chunk);
                t.zsecs += start.elapsed().as_secs_f64();
                t.zbytes += chunk.len() as u64;
                let d = with_dict.as_mut().map_or(p, |c| stored(c, chunk));
                let len = chunk.len() as u64;
                if first_in_file {
                    fresh_new += 1;
                    t.fresh.raw += len;
                    t.fresh.plain += p;
                    t.fresh.dict += d;
                }
                if new_to_store {
                    version_new += 1;
                    t.version.raw += len;
                    t.version.plain += p;
                    t.version.dict += d;
                }
            }
            if !matches!(method, Method::Whole) {
                let refs = new_chunks.len() as u64;
                if !unchanged {
                    t.version.overhead += version_new * INDEX_ENTRY + refs * MANIFEST_ENTRY;
                }
                t.fresh.overhead += fresh_new * INDEX_ENTRY + refs * MANIFEST_ENTRY;
            }
        }
    }

    let pairs = files.len() / 2;
    let speed = |t: &ZTally| {
        if t.zsecs > 0.0 {
            t.zbytes as f64 / t.zsecs / 1e6
        } else {
            0.0
        }
    };
    let cell = |bytes: u64| {
        format!(
            "{} MiB ({:.1}%)",
            mib(bytes),
            100.0 * bytes as f64 / input.max(1) as f64
        )
    };
    println!("### {label}\n");
    println!(
        "{pairs} pairs, {} MiB of new versions. zstd level {}; dictionaries of {} KiB, \
         each trained on its pair's old version. Percentages are of the new version's \
         size, uncompressed. Chunk lists and index entries are included, uncompressed.\n",
        mib(input),
        level(),
        dict_size() >> 10
    );
    for (title, pick) in [
        ("The new version (what the old version lacks):", true),
        ("The new file stored fresh (every distinct chunk):", false),
    ] {
        println!("{title}\n");
        println!("| method | uncompressed | zstd | zstd + dictionary | zstd MB/s |");
        println!("|---|---:|---:|---:|---:|");
        for (method, t) in ZMETHODS.iter().zip(&tallies) {
            let c = if pick { &t.version } else { &t.fresh };
            let dict = if matches!(method, Method::Whole) {
                "–".to_string()
            } else {
                cell(c.dict + c.overhead)
            };
            println!(
                "| {} | {} | {} | {} | {:.0} |",
                method.name(),
                cell(c.raw + c.overhead),
                cell(c.plain + c.overhead),
                dict,
                speed(t)
            );
        }
        println!();
    }
}
