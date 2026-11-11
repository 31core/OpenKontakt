//! FastLZ level 1 and 2 decompression.
//!
//! Ported from the reference C implementation with every level-2 feature described in
//! `docs/format.md` §2.6. The output length is always compared by the caller.

use crate::error::{Error, Result};

const MAX_L2_DISTANCE: usize = 8191;

/// Decompress a FastLZ level 1 or level 2 stream.
///
/// `max_output` protects against hostile expanded sizes; callers should pass the declared
/// `expanded_size` from the SubtreeItem.
pub fn decompress(src: &[u8], max_output: usize) -> Result<Vec<u8>> {
    if src.is_empty() {
        return Err(fastlz_error(0, "empty stream"));
    }
    let level = (src[0] >> 5) + 1;
    if !matches!(level, 1 | 2) {
        return Err(fastlz_error(0, format!("unknown level {level}")));
    }

    let n = src.len();
    let mut ip = 1usize;
    let mut out: Vec<u8> = Vec::with_capacity(max_output.min(1 << 26));
    let mut ctrl = (src[0] & 31) as usize;
    let ip_limit = n;
    let ip_bound = n.saturating_sub(2);

    loop {
        if ctrl >= 32 {
            let mut length = (ctrl >> 5) - 1;
            let ofs = (ctrl & 31) << 8;
            let mut ref_pos = out.len() as isize - ofs as isize - 1;

            if length == 6 {
                if ip > ip_bound {
                    return Err(fastlz_error(out.len(), "long-match length runs past input"));
                }
                loop {
                    let code = src[ip];
                    ip += 1;
                    length += code as usize;
                    if code != 0xFF {
                        break;
                    }
                    if ip > ip_bound {
                        return Err(fastlz_error(out.len(), "unterminated long-match length"));
                    }
                }
            }

            if ip >= n {
                return Err(fastlz_error(out.len(), "missing match distance byte"));
            }
            let code = src[ip];
            ip += 1;
            ref_pos -= code as isize;
            length += 3;

            if level == 2 && code == 0xFF && ofs == (31 << 8) {
                if ip + 1 >= n {
                    return Err(fastlz_error(out.len(), "truncated 16-bit distance escape"));
                }
                let mut dist = (src[ip] as usize) << 8;
                ip += 1;
                dist += src[ip] as usize;
                ip += 1;
                ref_pos = out.len() as isize - dist as isize - MAX_L2_DISTANCE as isize - 1;
            }

            if ref_pos < 0 || length > max_output.saturating_sub(out.len()) {
                return Err(fastlz_error(out.len(), "invalid back-reference"));
            }
            let mut rp = ref_pos as usize;
            for _ in 0..length {
                let b = out[rp];
                out.push(b);
                rp += 1;
            }
        } else {
            let length = ctrl + 1;
            if ip + length > n {
                return Err(fastlz_error(out.len(), "literal run runs past input"));
            }
            if length > max_output.saturating_sub(out.len()) {
                return Err(fastlz_error(out.len(), "output exceeds declared size"));
            }
            out.extend_from_slice(&src[ip..ip + length]);
            ip += length;
        }

        if level == 2 {
            if ip >= ip_limit {
                break;
            }
        } else if ip > ip_bound {
            break;
        }
        ctrl = src[ip] as usize;
        ip += 1;
    }

    Ok(out)
}

fn fastlz_error(produced: usize, reason: impl Into<String>) -> Error {
    Error::FastLz {
        expected: 0,
        produced,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_only() {
        // Level 1, one control byte with a 2-byte literal run.
        let out = decompress(&[0x01, b'h', b'i'], 10).unwrap();
        assert_eq!(out, b"hi");
    }
}
