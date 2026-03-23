// utils.rs — Shared utility functions

/// UTF-8 safe slicing: return the longest prefix of `s` that is at most `max` bytes,
/// without splitting a multi-byte character.
pub fn safe_slice_end(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut e = max;
    while e > 0 && !s.is_char_boundary(e) {
        e -= 1;
    }
    &s[..e]
}

/// UTF-8 safe slicing: return the longest suffix of `s` that is at most `max` bytes,
/// without splitting a multi-byte character.
pub fn safe_slice_start(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut s_idx = s.len() - max;
    while s_idx < s.len() && !s.is_char_boundary(s_idx) {
        s_idx += 1;
    }
    &s[s_idx..]
}
