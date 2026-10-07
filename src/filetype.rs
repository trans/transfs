//! How a file is stored, decided from its first bytes and its size
//! (docs/file-types.md).

/// Files smaller than this stay whole blobs: a chunk list would cost about
/// what chunking saves. (A guess; docs/file-types.md lists it as unmeasured.)
pub const SMALL_FILE: u64 = 32 << 10;

/// How many leading bytes [`layout`] needs to recognize a file.
pub const HEAD: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One whole blob.
    Whole,
    /// Content-defined chunks (FastCDC).
    FastCdc,
    /// Fixed chunks of this many bytes: a SQLite database's pages.
    Pages(u32),
}

/// The layout for a file of `len` bytes starting with `head`.
pub fn layout(head: &[u8], len: u64) -> Layout {
    if len < SMALL_FILE {
        return Layout::Whole;
    }
    if let Some(page) = sqlite_page_size(head) {
        if len.is_multiple_of(u64::from(page)) {
            return Layout::Pages(page);
        }
    }
    if already_compressed(head) {
        return Layout::Whole;
    }
    Layout::FastCdc
}

/// A SQLite database's page size, from bytes 16–17 of its header (a stored 1
/// means 65,536).
pub fn sqlite_page_size(head: &[u8]) -> Option<u32> {
    if head.len() < 18 || !head.starts_with(b"SQLite format 3\0") {
        return None;
    }
    let page = match u16::from_be_bytes([head[16], head[17]]) {
        1 => 65_536,
        n => u32::from(n),
    };
    (page.is_power_of_two() && (512..=65_536).contains(&page)).then_some(page)
}

/// Media and archives that are already compressed. Chunking saves them
/// nothing, and as whole blobs they stay real files that programs can open
/// by path.
fn already_compressed(head: &[u8]) -> bool {
    let at = |offset: usize, signature: &[u8]| {
        head.get(offset..offset + signature.len()) == Some(signature)
    };
    (at(0, b"RIFF") && at(8, b"WEBP"))
        || at(0, &[0xFF, 0xD8, 0xFF]) // JPEG
        || at(0, b"GIF87a")
        || at(0, b"GIF89a")
        || at(4, b"ftyp") // MP4, MOV, HEIC, AVIF
        || at(0, &[0x1A, 0x45, 0xDF, 0xA3]) // Matroska, WebM
        || at(0, b"ID3") // MP3
        || at(0, b"OggS")
        || at(0, b"fLaC")
        || at(0, b"PK\x03\x04") // zip, and documents built on it
        || at(0, &[0x1F, 0x8B]) // gzip
        || at(0, &[0x28, 0xB5, 0x2F, 0xFD]) // zstd
        || at(0, &[0xFD, b'7', b'z', b'X', b'Z', 0x00]) // xz
        || at(0, b"BZh")
        || at(0, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) // 7-Zip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_head(raw_page: u16) -> Vec<u8> {
        let mut head = b"SQLite format 3\0".to_vec();
        head.extend_from_slice(&raw_page.to_be_bytes());
        head.resize(HEAD, 0);
        head
    }

    #[test]
    fn small_files_stay_whole() {
        assert_eq!(layout(&sqlite_head(4096), 4096), Layout::Whole);
        assert_eq!(layout(b"plain text", SMALL_FILE - 1), Layout::Whole);
    }

    #[test]
    fn sqlite_is_cut_into_pages() {
        assert_eq!(layout(&sqlite_head(4096), 1 << 20), Layout::Pages(4096));
        assert_eq!(layout(&sqlite_head(1), 1 << 20), Layout::Pages(65_536));
        // Not a whole number of pages, or an invalid page size: content-defined.
        assert_eq!(layout(&sqlite_head(4096), (1 << 20) + 1), Layout::FastCdc);
        assert_eq!(layout(&sqlite_head(1000), 1 << 20), Layout::FastCdc);
    }

    #[test]
    fn compressed_media_stay_whole_and_others_are_chunked() {
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.resize(HEAD, 0);
        assert_eq!(layout(&webp, 1 << 20), Layout::Whole);
        assert_eq!(layout(&[0xFF, 0xD8, 0xFF, 0xE0], 1 << 20), Layout::Whole);
        assert_eq!(layout(b"gimp xcf v022\0", 1 << 20), Layout::FastCdc);
        assert_eq!(layout(b"\x89PNG\r\n\x1a\n", 1 << 20), Layout::FastCdc);
        assert_eq!(layout(b"plain text", 1 << 20), Layout::FastCdc);
    }
}
