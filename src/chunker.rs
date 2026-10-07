//! Cutting a file into chunks: FastCDC for most files, fixed pages for SQLite
//! databases (docs/chunked-storage.md, "Format parameters"). These parameters
//! are part of the store format: writers share chunks only if they cut the
//! same way.
use crate::{filetype::Layout, Error, Result};
use fastcdc::v2020::{Normalization, StreamCDC};
use std::io::{self, Read};

pub const MIN: u32 = 2 << 10;
pub const AVG: u32 = 8 << 10;
pub const MAX: u32 = 32 << 10;

/// The chunking a representation record names.
pub fn name(layout: Layout) -> String {
    match layout {
        Layout::FastCdc => format!("fastcdc-2020/{MIN}/{AVG}/{MAX}"),
        Layout::Pages(page) => format!("fixed/{page}"),
        Layout::Whole => "whole".into(),
    }
}

/// Calls `each` with every chunk of `source`, in order, reading it as a
/// stream so files larger than memory work.
pub fn chunks(
    mut source: impl Read,
    layout: Layout,
    mut each: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    match layout {
        Layout::FastCdc => {
            let chunker =
                StreamCDC::with_level_and_seed(source, MIN, AVG, MAX, Normalization::Level1, 0);
            for chunk in chunker {
                let chunk = chunk.map_err(|e| Error::Storage(format!("chunking failed: {e:?}")))?;
                each(&chunk.data)?;
            }
        }
        Layout::Pages(page) => {
            let mut buffer = vec![0; page as usize];
            loop {
                let n = read_full(&mut source, &mut buffer)?;
                if n == 0 {
                    break;
                }
                each(&buffer[..n])?;
                if n < buffer.len() {
                    break;
                }
            }
        }
        Layout::Whole => {
            let mut all = Vec::new();
            source.read_to_end(&mut all)?;
            each(&all)?;
        }
    }
    Ok(())
}

/// Fills `buffer` unless the source ends first; returns how much was read.
pub fn read_full(source: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize, mut seed: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect()
    }

    fn lengths(bytes: &[u8], layout: Layout) -> Vec<usize> {
        let mut out = Vec::new();
        chunks(bytes, layout, |chunk| {
            out.push(chunk.len());
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn fastcdc_boundaries_are_pinned() {
        // Golden values: a change here means chunks no longer match those
        // already stored, which would silently stop versions sharing.
        let lengths = lengths(&data(1 << 20, 7), Layout::FastCdc);
        assert_eq!(lengths.iter().sum::<usize>(), 1 << 20);
        assert!(lengths.iter().all(|&n| n <= MAX as usize));
        assert_eq!(lengths.len(), GOLDEN_COUNT);
        assert_eq!(&lengths[..4], &GOLDEN_FIRST);
    }

    const GOLDEN_COUNT: usize = 104;
    const GOLDEN_FIRST: [usize; 4] = [10689, 2649, 9195, 5302];

    #[test]
    fn streaming_cuts_where_the_chunk_study_did() {
        // The chunk study measured with the in-memory chunker; transfs stores
        // with the streaming one. Both must cut at the same places.
        let bytes = data(3 << 20, 11);
        let in_memory: Vec<usize> = fastcdc::v2020::FastCDC::new(&bytes, MIN, AVG, MAX)
            .map(|chunk| chunk.length)
            .collect();
        assert_eq!(lengths(&bytes, Layout::FastCdc), in_memory);
    }

    #[test]
    fn insertions_reuse_the_following_chunks() {
        let original = data(1 << 20, 9);
        let mut edited = original.clone();
        edited.splice(1000..1000, *b"inserted near the start");
        let before = lengths(&original, Layout::FastCdc);
        let after = lengths(&edited, Layout::FastCdc);
        // After the first chunk or two, the cut points line up again.
        assert_eq!(before[3..], after[3..]);
    }

    #[test]
    fn pages_are_fixed() {
        assert_eq!(
            lengths(&data(10_000, 1), Layout::Pages(4096)),
            [4096, 4096, 1808]
        );
        assert_eq!(lengths(&data(8192, 1), Layout::Pages(4096)), [4096, 4096]);
        assert!(lengths(&[], Layout::Pages(4096)).is_empty());
    }
}
