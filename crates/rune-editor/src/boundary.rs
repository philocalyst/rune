//! Checked conversion at the legacy Lisp boundary only. Marker resolution and
//! Lisp type/domain checks must happen in the owning runtime before calling.
use crate::{EditorError, Result};

pub fn from_lisp_point(position: i64, len_chars: usize) -> Result<usize> {
    let offset = position.checked_sub(1).and_then(|value| usize::try_from(value).ok())
        .ok_or(EditorError::InvalidPosition)?;
    if offset > len_chars { return Err(EditorError::InvalidPosition); }
    Ok(offset)
}
pub fn to_lisp_point(offset: usize, len_chars: usize) -> Result<i64> {
    if offset > len_chars { return Err(EditorError::InvalidPosition); }
    offset.checked_add(1).and_then(|point| i64::try_from(point).ok())
        .ok_or(EditorError::InvalidPosition)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_scalar_endpoint_and_empty_buffer_round_trip() {
        let length = "λ🙂x".chars().count();
        for offset in 0..=length {
            assert_eq!(from_lisp_point(to_lisp_point(offset, length).unwrap(), length), Ok(offset));
        }
        assert_eq!(from_lisp_point(1, 0), Ok(0));
        assert_eq!(to_lisp_point(0, 0), Ok(1));
    }
    #[test]
    fn invalid_signed_and_overflow_positions_never_clamp_or_wrap() {
        for point in [i64::MIN, -1, 0, 5, i64::MAX] {
            assert_eq!(from_lisp_point(point, 3), Err(EditorError::InvalidPosition));
        }
        assert_eq!(to_lisp_point(usize::MAX, usize::MAX), Err(EditorError::InvalidPosition));
        assert_eq!(to_lisp_point(4, 3), Err(EditorError::InvalidPosition));
    }
}
