//! Columns for tables a person reads.  `format!("{:<n}")` counts chars, but a
//! Chinese character (an agent named 星, a translated cell) takes two
//! terminal columns, so a padded table would drift out of line.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// `text` and enough spaces after it to fill `columns` terminal columns; as
/// it is when it is already wider.
pub fn pad(text: &str, columns: usize) -> String {
    let width = text.width();
    let mut out = String::with_capacity(text.len() + columns.saturating_sub(width));
    out.push_str(text);
    out.extend(std::iter::repeat_n(' ', columns.saturating_sub(width)));
    assert!(out.width() >= columns, "padded to at least the column width");
    out
}

/// `text` right-aligned in `columns` terminal columns.
pub fn pad_left(text: &str, columns: usize) -> String {
    let width = text.width();
    let mut out: String = std::iter::repeat_n(' ', columns.saturating_sub(width)).collect();
    out.push_str(text);
    assert!(out.width() >= columns, "padded to at least the column width");
    out
}

/// `text` cut to `columns` terminal columns, its last one an ellipsis when cut.
pub fn fit(text: &str, columns: usize) -> String {
    if text.width() <= columns {
        return text.to_string();
    }
    let room = columns.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    assert!(out.width() <= columns.max(1), "a cut cell stays inside its column");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chinese_cell_is_padded_by_the_columns_it_takes() {
        // Two characters, four columns: six spaces make ten.
        assert_eq!(pad("星辰", 10), "星辰      ");
        assert_eq!(pad("星辰", 10).width(), pad("atlas", 10).width());
        assert_eq!(pad_left("未读", 6), "  未读");
        assert_eq!(pad("too wide for it", 4), "too wide for it", "a wide cell is left whole");
    }

    #[test]
    fn a_cut_cell_ends_in_an_ellipsis_inside_its_column() {
        assert_eq!(fit("atlas", 8), "atlas");
        assert_eq!(fit("atlas,nova,vega", 8), "atlas,n…");
        // 星 takes two columns: three fit in seven, then the ellipsis.
        assert_eq!(fit("星辰大海星辰", 8), "星辰大…");
        assert!(fit("星辰大海星辰", 8).width() <= 8);
    }
}
