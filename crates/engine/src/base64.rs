//! Base64 (RFC 4648, with padding): how bytes travel in JSON, such as a PNG picture or a zipped
//! project.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Bytes as base64.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            let digit = ALPHABET.get(((n >> shift) & 63) as usize).copied().unwrap_or(b'=');
            out.push(if i <= chunk.len() { char::from(digit) } else { '=' });
        }
    }
    out
}

/// The bytes base64 text stands for, or `None` for text that isn't base64: a character outside
/// the alphabet, a length that isn't a multiple of four, or padding anywhere but at the end.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| ALPHABET.iter().position(|&a| a == c).and_then(|v| u32::try_from(v).ok());
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let quads = text.len() / 4;
    for (q, quad) in text.chunks(4).enumerate() {
        let padding = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if padding > 2 || (padding > 0 && q + 1 != quads) {
            return None;
        }
        let mut n: u32 = 0;
        for &c in quad.iter().take(4 - padding) {
            n = (n << 6) | value(c)?;
        }
        n <<= 6 * padding as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend(bytes.iter().take(3 - padding));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_alphabet_with_padding_both_ways() {
        let cases: [(&[u8], &str); 6] =
            [(b"", ""), (b"f", "Zg=="), (b"fo", "Zm8="), (b"foo", "Zm9v"), (b"foob", "Zm9vYg=="), (&[0xff, 0xfe, 0xfd], "//79")];
        for (bytes, text) in cases {
            assert_eq!(encode(bytes), text);
            assert_eq!(decode(text).as_deref(), Some(bytes), "{text}");
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&all)), Some(all));
    }

    #[test]
    fn text_that_is_not_base64_is_refused() {
        for bad in ["Zg=", "Zg", "Z===", "Zg==Zg==", "Zm9v!", "Zm 9v", "é===", "===="] {
            assert_eq!(decode(bad), None, "{bad}");
        }
    }
}
