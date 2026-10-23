//! Decoding text the way Windows tools produce it.
//!
//! Solution and project files are UTF-8 (usually with a BOM), occasionally UTF-16 with a BOM, and
//! old ones are in the system's ANSI code page. Console programs write in the console code page,
//! which is CP932 on a Japanese Windows and UTF-8 only when the system has "Use Unicode UTF-8" set.

use std::io;
use std::path::Path;

/// Decodes a file's bytes: UTF-8 (BOM optional), UTF-16 LE/BE with a BOM, otherwise the ANSI
/// code page.
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => code_page(bytes, CodePage::Ansi),
    }
}

fn utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| read([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// Reads and decodes a text file (see [`decode`]).
pub fn read_text(path: &Path) -> io::Result<String> {
    Ok(decode(&std::fs::read(path)?))
}

/// Which code page to fall back to for bytes that are not UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodePage {
    /// The system ANSI code page (`GetACP`): files written by non-Unicode programs.
    Ansi,
    /// The OEM code page (`GetOEMCP`): what a console program writes to a new console.
    Oem,
    /// An explicit code page number.
    Number(u32),
}

/// Decodes one line of a console program's output: UTF-8 when the bytes are valid UTF-8
/// (text in the other code pages almost never is), otherwise `fallback`.
pub fn decode_line(bytes: &[u8], fallback: CodePage) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => code_page(bytes, fallback),
    }
}

#[cfg(windows)]
fn code_page(bytes: &[u8], page: CodePage) -> String {
    let number = match page {
        CodePage::Ansi => crate::win::ansi_code_page(),
        CodePage::Oem => crate::win::oem_code_page(),
        CodePage::Number(n) => n,
    };
    crate::win::multi_byte_to_string(number, bytes)
}

#[cfg(not(windows))]
fn code_page(bytes: &[u8], _page: CodePage) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Splits text into lines, accepting `\r\n`, `\n` and a lone `\r`.
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_boms() {
        assert_eq!(decode(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(decode(&[0xFF, 0xFE, b'a', 0, b'b', 0]), "ab");
        assert_eq!(decode(&[0xFE, 0xFF, 0, b'a', 0, b'b']), "ab");
        assert_eq!(decode("テスト".as_bytes()), "テスト");
    }

    #[cfg(windows)]
    #[test]
    fn falls_back_to_a_code_page() {
        // "テスト" in CP932.
        let sjis = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67];
        assert_eq!(decode_line(&sjis, CodePage::Number(932)), "テスト");
        assert_eq!(decode_line("テスト".as_bytes(), CodePage::Number(932)), "テスト");
    }

    #[test]
    fn splits_any_line_ending() {
        let v: Vec<&str> = lines("a\r\nb\nc").collect();
        assert_eq!(v, ["a", "b", "c"]);
    }
}
