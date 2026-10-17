//! JS 字符串的**索引单位是 UTF-16 码元**，而本引擎内部存的是 Rust `String`
//! （UTF-8，按字符/码点遍历）。
//!
//! 于是所有"按位置"的操作都差一层：`"\u{1F600}".length` 曾经是 1（应为 2）、
//! `charCodeAt(0)` 给出码点 128512（应为高代理 55357）、`indexOf` 报的是码点
//! 偏移（应为码元偏移）、`slice(0,1)` 把整个 emoji 当成一个单位。
//!
//! 这里提供唯一的换算层：需要下标语义的地方一律走 `units()` / `length()`，
//! 需要构造字符串的地方走 `from_units()`。**残留**：Rust `String` 装不下孤立
//! 代理（`"\uD83D"`），`from_units` 只能给 U+FFFD —— 那一族用例（`returns-code-unit`
//! 之类）仍然失败，属于"存储模型"层面的偏差，不是索引层能补救的。

/// 字符串的 UTF-16 码元序列。
pub(crate) fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `String.prototype.length`：UTF-16 码元个数（不是字节数、也不是码点数）。
pub(crate) fn length(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 由码元构造字符串。孤立代理会被折成 U+FFFD（见模块注释）。
pub(crate) fn from_units(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `s.slice(start, end)` 的码元切片。
pub(crate) fn slice(s: &str, start: usize, end: usize) -> String {
    let units = units(s);
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    from_units(&units[start..end])
}

/// `s[i]` / `charAt(i)`：单个码元（超出范围给空串）。
pub(crate) fn char_at(s: &str, index: usize) -> String {
    let units = units(s);
    units
        .get(index)
        .map(|u| from_units(&[*u]))
        .unwrap_or_default()
}

/// `s.charCodeAt(i)`；越界是 `None`（调用方给 `NaN`）。
pub(crate) fn char_code_at(s: &str, index: usize) -> Option<u16> {
    s.encode_utf16().nth(index)
}

/// `s.codePointAt(i)`：从第 `index` 个码元起读一个码点（代理对合并）。
pub(crate) fn code_point_at(s: &str, index: usize) -> Option<u32> {
    let units: Vec<u16> = units(s);
    let first = *units.get(index)?;
    // 高代理 + 低代理 → 一个码点；否则就是这一个码元。
    if (0xD800..=0xDBFF).contains(&first)
        && let Some(&second) = units.get(index + 1)
        && (0xDC00..=0xDFFF).contains(&second)
    {
        let hi = (first as u32) - 0xD800;
        let lo = (second as u32) - 0xDC00;
        return Some(0x10000 + (hi << 10) + lo);
    }
    Some(first as u32)
}

/// 码点偏移在"码点序列"里的位置 → 对应的码元偏移（`indexOf` 之类要用）。
pub(crate) fn unit_offset_of_code_point(s: &str, code_point_offset: usize) -> usize {
    s.chars()
        .take(code_point_offset)
        .map(|c| c.len_utf16())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_counts_code_units() {
        assert_eq!(length("abc"), 3);
        assert_eq!(length("éé"), 2);
        assert_eq!(length("中文"), 2);
        // 一个非 BMP 字符 = 两个码元。
        assert_eq!(length("\u{1F600}"), 2);
    }

    #[test]
    fn char_code_at_splits_surrogate_pairs() {
        assert_eq!(char_code_at("\u{1F600}", 0), Some(0xD83D));
        assert_eq!(char_code_at("\u{1F600}", 1), Some(0xDE00));
        assert_eq!(char_code_at("é", 0), Some(0xE9));
        assert_eq!(char_code_at("a", 1), None);
    }

    #[test]
    fn code_point_at_merges_the_pair() {
        assert_eq!(code_point_at("\u{1F600}", 0), Some(0x1F600));
        // 从低代理读出来的是低代理本身。
        assert_eq!(code_point_at("\u{1F600}", 1), Some(0xDE00));
        assert_eq!(code_point_at("a", 0), Some(0x61));
    }

    #[test]
    fn slice_uses_code_units() {
        // 切在代理对中间：孤立代理只能折成 U+FFFD（存储模型的残留）。
        assert_eq!(slice("a\u{1F600}b", 0, 2), "a\u{FFFD}");
        // 码元下标 3 才是 "b"：0='a'、1=高代理、2=低代理、3='b'。
        assert_eq!(slice("a\u{1F600}b", 3, 4), "b");
        assert_eq!(slice("abc", 1, 2), "b");
        assert_eq!(slice("abc", 2, 99), "c");
    }

    #[test]
    fn code_point_offsets_convert_to_unit_offsets() {
        assert_eq!(unit_offset_of_code_point("a\u{1F600}b", 2), 3);
        assert_eq!(unit_offset_of_code_point("abc", 2), 2);
    }
}
