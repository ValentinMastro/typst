use std::fmt::{self, Debug, Formatter};
use std::ops::Range;

use serde::{Deserialize, Serialize};
use ttf_parser::{PlatformId, name_id};

use super::find_exception;
use crate::text::{
    AxisValue, FontAxis, FontStretch, FontStyle, FontVariant, FontWeight, Tag,
};

/// Properties of a single font.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
pub struct FontInfo {
    /// The typographic font family this font is part of.
    pub family: String,
    /// Properties that distinguish this font from other fonts in the same
    /// family. For a variable font, this designates the default instance.
    pub variant: FontVariant,
    /// Properties of the font.
    pub flags: FontFlags,
    /// Variation axes this font supports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axes: Vec<FontAxis>,
    /// The unicode coverage of the font.
    pub coverage: Coverage,
}

bitflags::bitflags! {
    /// Bitflags describing characteristics of a font.
    #[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
    #[derive(Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct FontFlags: u32 {
        /// All glyphs have the same width.
        const MONOSPACE = 1 << 0;
        /// Glyphs have short strokes at their stems.
        const SERIF = 1 << 1;
        /// Font face has a MATH table
        const MATH = 1 << 2;
        /// Font face has an fvar table
        const VARIABLE = 1 << 3;
    }
}

impl FontInfo {
    /// Compute metadata for font at the `index` of the given data.
    pub fn new(data: &[u8], index: u32) -> Option<Self> {
        let ttf = ttf_parser::Face::parse(data, index).ok()?;
        Self::from_ttf(&ttf)
    }

    /// Compute metadata for all fonts in the given data.
    pub fn iter(data: &[u8]) -> impl Iterator<Item = FontInfo> + '_ {
        let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
        (0..count).filter_map(move |index| Self::new(data, index))
    }

    /// Compute metadata for a single ttf-parser face.
    pub(super) fn from_ttf(ttf: &ttf_parser::Face) -> Option<Self> {
        let ps_name = find_name(ttf, name_id::POST_SCRIPT_NAME);
        let exception = ps_name.as_deref().and_then(find_exception);
        // We cannot use Name ID 16 "Typographic Family", because for some
        // fonts it groups together more than just Style / Weight / Stretch
        // variants (e.g. Display variants of Noto fonts) and then some
        // variants become inaccessible from Typst. And even though the
        // fsSelection bit WWS should help us decide whether that is the
        // case, it's wrong for some fonts (e.g. for certain variants of "Noto
        // Sans Display").
        //
        // So, instead we use Name ID 1 "Family" and trim many common
        // suffixes for which know that they just describe styling (e.g.
        // "ExtraBold").
        let family =
            exception.and_then(|c| c.family.map(str::to_string)).or_else(|| {
                let family = find_name(ttf, name_id::FAMILY)?;
                Some(typographic_family(&family).to_string())
            })?;

        let variant = {
            let style = exception.and_then(|c| c.style).unwrap_or_else(|| {
                let mut full = find_name(ttf, name_id::FULL_NAME).unwrap_or_default();
                full.make_ascii_lowercase();

                // Some fonts miss the relevant bits for italic or oblique, so
                // we also try to infer that from the full name.
                //
                // We do not use `ttf.is_italic()` because that also checks the
                // italic angle which leads to false positives for some oblique
                // fonts.
                //
                // See <https://github.com/typst/typst/issues/7479>.
                let italic =
                    ttf.style() == ttf_parser::Style::Italic || full.contains("italic");
                let oblique = ttf.is_oblique()
                    || full.contains("oblique")
                    || full.contains("slanted");

                match (italic, oblique) {
                    (false, false) => FontStyle::Normal,
                    (true, _) => FontStyle::Italic,
                    (_, true) => FontStyle::Oblique,
                }
            });

            let weight = exception.and_then(|c| c.weight).unwrap_or_else(|| {
                let number = ttf.weight().to_number();
                FontWeight::from_number(number)
            });

            let stretch = exception
                .and_then(|c| c.stretch)
                .unwrap_or_else(|| FontStretch::from_number(ttf.width().to_number()));

            FontVariant { style, weight, stretch }
        };

        // Determine the unicode coverage.
        let mut coverage = Coverage::default();
        if let Some(cmap) = ttf.tables().cmap {
            let raw = ttf.raw_face().table(ttf_parser::Tag::from_bytes(b"cmap"));
            for (index, subtable) in cmap.subtables.into_iter().enumerate() {
                if subtable.is_unicode() {
                    let mut subtable_coverage =
                        CoverageBuilder::with_capacity(coverage.0.len());
                    // We expect codepoints in CMAP subtables to be already sorted
                    if !raw.is_some_and(|raw| {
                        cmap_ranges(raw, index, |range| {
                            subtable_coverage.add_codepoint_range(range);
                        })
                    }) {
                        subtable.codepoints(|c| subtable_coverage.add_codepoint(c));
                    }
                    coverage = coverage.union(&subtable_coverage.build());
                }
            }
        }

        let mut flags = FontFlags::empty();
        flags.set(FontFlags::MONOSPACE, ttf.is_monospaced());
        flags.set(FontFlags::MATH, ttf.tables().math.is_some());
        flags.set(FontFlags::VARIABLE, ttf.is_variable());

        // Determine whether this is a serif or sans-serif font.
        if let Some(panose) = ttf
            .raw_face()
            .table(ttf_parser::Tag::from_bytes(b"OS/2"))
            .and_then(|os2| os2.get(32..45))
            && matches!(panose, [2, 2..=10, ..])
        {
            flags.insert(FontFlags::SERIF);
        }

        let axes = ttf
            .variation_axes()
            .into_iter()
            .map(|axis| FontAxis {
                tag: Tag::from_bytes(&axis.tag.to_bytes()),
                min: AxisValue(axis.min_value),
                max: AxisValue(axis.max_value),
                default: AxisValue(axis.def_value),
            })
            .collect();

        Some(FontInfo { family, variant, flags, axes, coverage })
    }

    /// Whether this is the macOS LastResort font. It can yield tofus with
    /// glyph ID != 0.
    pub fn is_last_resort(&self) -> bool {
        self.family == "LastResort"
    }
}

/// Try to find and decode the name with the given id.
pub(super) fn find_name(ttf: &ttf_parser::Face, name_id: u16) -> Option<String> {
    ttf.names().into_iter().find_map(|entry| {
        if entry.name_id == name_id {
            if let Some(string) = entry.to_string() {
                return Some(string);
            }

            if entry.platform_id == PlatformId::Macintosh && entry.encoding_id == 0 {
                return Some(decode_mac_roman(entry.name));
            }
        }

        None
    })
}

/// Decode mac roman encoded bytes into a string.
fn decode_mac_roman(coded: &[u8]) -> String {
    #[rustfmt::skip]
    const TABLE: [char; 128] = [
        'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è',
        'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü',
        '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø',
        '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø',
        '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', '\u{a0}', 'À', 'Ã', 'Õ', 'Œ', 'œ',
        '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ',
        '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô',
        '\u{f8ff}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
    ];

    fn char_from_mac_roman(code: u8) -> char {
        if code < 128 { code as char } else { TABLE[(code - 128) as usize] }
    }

    coded.iter().copied().map(char_from_mac_roman).collect()
}

/// Trim style naming from a family name and fix bad names.
fn typographic_family(mut family: &str) -> &str {
    // Separators between names, modifiers and styles.
    const SEPARATORS: [char; 3] = [' ', '-', '_'];

    // Modifiers that can appear in combination with suffixes.
    const MODIFIERS: &[&str] =
        &["extra", "ext", "ex", "x", "semi", "sem", "sm", "demi", "dem", "ultra"];

    // Style suffixes.
    #[rustfmt::skip]
    const SUFFIXES: &[&str] = &[
        "normal", "italic", "oblique", "slanted",
        "thin", "th", "hairline", "light", "lt", "regular", "medium", "med",
        "md", "bold", "bd", "demi", "extb", "black", "blk", "bk", "heavy",
        "narrow", "condensed", "cond", "cn", "cd", "compressed", "expanded", "exp",
        "vf", "var", "variable",
    ];

    // Trim spacing and weird leading dots in Apple fonts.
    family = family.trim().trim_start_matches('.');

    // Lowercase the string so that the suffixes match case-insensitively.
    let lower = family.to_ascii_lowercase();
    let mut len = usize::MAX;
    let mut trimmed = lower.as_str();

    // Trim style suffixes repeatedly.
    while trimmed.len() < len {
        len = trimmed.len();

        // Find style suffix.
        let mut t = trimmed;
        let mut shortened = false;
        while let Some(s) = SUFFIXES.iter().find_map(|s| t.strip_suffix(s)) {
            shortened = true;
            t = s;
        }

        if !shortened {
            break;
        }

        // Strip optional separator.
        if let Some(s) = t.strip_suffix(SEPARATORS) {
            trimmed = s;
            t = s;
        }

        // Also allow an extra modifier, but apply it only if it is separated it
        // from the text before it (to prevent false positives).
        if let Some(t) = MODIFIERS.iter().find_map(|s| t.strip_suffix(s))
            && let Some(stripped) = t.strip_suffix(SEPARATORS)
        {
            trimmed = stripped;
        }
    }

    // Apply style suffix trimming.
    family = &family[..len];

    family
}

/// Calls `f` with the ranges of codepoints mapped by the `index`-th subtable
/// of the raw `cmap` table, in the order in which they are defined.
///
/// This yields the same codepoints as `ttf_parser`'s `Subtable::codepoints`,
/// but range by range instead of codepoint by codepoint, which is much faster
/// for large fonts. Only formats 4 and 12 are supported. Returns `false`
/// without calling `f` if the subtable has another format or is malformed.
fn cmap_ranges(cmap: &[u8], index: usize, mut f: impl FnMut(Range<u32>)) -> bool {
    let u16_at = |data: &[u8], at: usize| {
        data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    };
    let u32_at = |data: &[u8], at: usize| {
        data.get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };

    // The encoding records start after the version and the number of tables.
    // Each record consists of a platform ID, an encoding ID and an offset.
    let Some(data) = u32_at(cmap, 4 + 8 * index + 4)
        .and_then(|offset| cmap.get(usize::try_from(offset).ok()?..))
    else {
        return false;
    };

    match u16_at(data, 0) {
        // Segment mapping to delta values.
        Some(4) => {
            let Some(seg_count) = u16_at(data, 6).map(|x2| usize::from(x2 / 2)) else {
                return false;
            };
            let ends = 14;
            let starts = ends + 2 * seg_count + 2;
            if data.len() < starts + 2 * seg_count {
                return false;
            }
            for i in 0..seg_count {
                let start = u16_at(data, starts + 2 * i).unwrap();
                let end = u16_at(data, ends + 2 * i).unwrap();
                // The 0xFFFF value is special and indicates the end of codes.
                if start == end && start == 0xFFFF {
                    break;
                }
                f(u32::from(start)..u32::from(end) + 1);
            }
            true
        }
        // Segmented coverage.
        Some(12) => {
            let Some(count) = u32_at(data, 12).and_then(|c| usize::try_from(c).ok())
            else {
                return false;
            };
            let groups = 16;
            if data.len().saturating_sub(groups) / 12 < count {
                return false;
            }
            for i in 0..count {
                let start = u32_at(data, groups + 12 * i).unwrap();
                let end = u32_at(data, groups + 12 * i + 4).unwrap();
                f(start..end.saturating_add(1));
            }
            true
        }
        _ => false,
    }
}

/// A compactly encoded set of codepoints.
///
/// The set is represented by alternating specifications of how many codepoints
/// are not in the set and how many are in the set.
///
/// For example, for the set `{2, 3, 4, 9, 10, 11, 15, 18, 19}`, there are:
/// - 2 codepoints not inside (0, 1)
/// - 3 codepoints inside (2, 3, 4)
/// - 4 codepoints not inside (5, 6, 7, 8)
/// - 3 codepoints inside (9, 10, 11)
/// - 3 codepoints not inside (12, 13, 14)
/// - 1 codepoint inside (15)
/// - 2 codepoints not inside (16, 17)
/// - 2 codepoints inside (18, 19)
///
/// So the resulting encoding is `[2, 3, 4, 3, 3, 1, 2, 2]`.
///
/// This is the serialized representation. In memory, the set is instead
/// stored as the sorted boundaries of its ranges, here
/// `[2, 5, 9, 12, 15, 16, 18, 20]`, so that membership can be tested with a
/// binary search.
#[derive(Default, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(from = "RunLengths", into = "RunLengths")]
pub struct Coverage(Vec<u32>);

/// The run-length encoding of a [`Coverage`] set, used for serialization.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct RunLengths(Vec<u32>);

impl From<RunLengths> for Coverage {
    fn from(RunLengths(runs): RunLengths) -> Self {
        let mut bounds = Vec::with_capacity(runs.len());
        let mut cursor = 0;
        for &[gap, count] in runs.as_chunks::<2>().0 {
            cursor += gap;
            bounds.push(cursor);
            cursor += count;
            bounds.push(cursor);
        }
        Self(bounds)
    }
}

impl From<Coverage> for RunLengths {
    fn from(Coverage(bounds): Coverage) -> Self {
        let mut runs = Vec::with_capacity(bounds.len());
        let mut cursor = 0;
        for &bound in &bounds {
            runs.push(bound - cursor);
            cursor = bound;
        }
        Self(runs)
    }
}

impl Coverage {
    /// Encode a vector of codepoints.
    pub fn from_vec(mut codepoints: Vec<u32>) -> Self {
        #[expect(
            clippy::stable_sort_primitive,
            reason = "`sort_unstable` hurts performance here. Compiling a 'Hello World!' \
            PDF with `--ignore-system-fonts` goes from 7.8ms to 9.1ms or +16%."
        )]
        codepoints.sort();
        codepoints.dedup();

        let mut builder = CoverageBuilder::with_capacity(0);
        for c in codepoints {
            builder.add_codepoint(c);
        }
        builder.build()
    }

    /// Whether the codepoint is covered.
    pub fn contains(&self, c: u32) -> bool {
        // The codepoint is inside a range if an odd number of boundaries is at
        // or before it, i.e. if the last of them is the start of a range.
        self.0.partition_point(|&bound| bound <= c) % 2 == 1
    }

    /// Iterate over all covered codepoints.
    pub fn iter(&self) -> impl Iterator<Item = u32> {
        self.iter_ranges().flatten()
    }

    /// Iterate over all continuous ranges of covered codepoints.
    fn iter_ranges(&self) -> impl Iterator<Item = Range<u32>> {
        let (pairs, _) = self.0.as_chunks::<2>();
        pairs.iter().map(|&[start, end]| start..end)
    }

    /// Returns an encoding of the set of codepoints covered by either `self` or
    /// `other`.
    fn union(&self, other: &Coverage) -> Self {
        let capacity = self.0.len().max(other.0.len());
        let mut builder = CoverageBuilder::with_capacity(capacity);

        let mut self_ranges = self.iter_ranges().peekable();
        let mut other_ranges = other.iter_ranges().peekable();

        while let Some(self_range) = self_ranges.peek()
            && let Some(other_range) = other_ranges.peek()
        {
            if self_range.start < other_range.start {
                builder.add_codepoint_range(self_range.clone());
                self_ranges.next();
            } else {
                builder.add_codepoint_range(other_range.clone());
                other_ranges.next();
            }
        }

        for self_range in self_ranges {
            builder.add_codepoint_range(self_range);
        }
        for other_range in other_ranges {
            builder.add_codepoint_range(other_range);
        }

        builder.build()
    }
}

impl Debug for Coverage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.pad("Coverage(..)")
    }
}

/// Incrementally builds a [`Coverage`] set from a non-decreasing series of
/// codepoints, or ranges thereof.
struct CoverageBuilder {
    bounds: Vec<u32>,
    next: u32,
}

impl CoverageBuilder {
    /// Create a new coverage builder with at least the given capacity.
    fn with_capacity(capacity: usize) -> Self {
        Self { bounds: Vec::with_capacity(capacity), next: 0 }
    }

    /// Build the [`Coverage`] set.
    fn build(self) -> Coverage {
        Coverage(self.bounds)
    }

    /// Add a single codepoint to the set being built. Codepoints must be added
    /// in a strictly increasing order.
    fn add_codepoint(&mut self, codepoint: u32) {
        let range = codepoint..(codepoint.saturating_add(1));
        self.add_codepoint_range(range);
    }

    /// Add a continuous range of codepoints to the set being built. Ranges must
    /// be added in non-decreasing order of start codepoint, but may intersect
    /// or be contained in previously added ranges.
    fn add_codepoint_range(&mut self, range: Range<u32>) {
        if range.end <= self.next || range.is_empty() {
            return;
        } else if range.start <= self.next
            && let Some(prev_end) = self.bounds.last_mut()
        {
            *prev_end = range.end;
        } else {
            self.bounds.extend([range.start, range.end]);
        }

        self.next = range.end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trim_styles() {
        assert_eq!(typographic_family("Atma Light"), "Atma");
        assert_eq!(typographic_family("eras bold"), "eras");
        assert_eq!(typographic_family("footlight mt light"), "footlight mt");
        assert_eq!(typographic_family("times new roman"), "times new roman");
        assert_eq!(typographic_family("noto sans mono cond sembd"), "noto sans mono");
        assert_eq!(typographic_family("noto serif SEMCOND sembd"), "noto serif");
        assert_eq!(typographic_family("crimson text"), "crimson text");
        assert_eq!(typographic_family("footlight light"), "footlight");
        assert_eq!(typographic_family("Noto Sans"), "Noto Sans");
        assert_eq!(typographic_family("Noto Sans Light"), "Noto Sans");
        assert_eq!(typographic_family("Noto Sans Semicondensed Heavy"), "Noto Sans");
        assert_eq!(typographic_family("Familx"), "Familx");
        assert_eq!(typographic_family("Font Ultra"), "Font Ultra");
        assert_eq!(typographic_family("Font Ultra Bold"), "Font");
    }

    #[test]
    fn test_coverage() {
        #[track_caller]
        fn test(set: &[u32], runs: &[u32]) {
            let coverage = Coverage::from_vec(set.to_vec());
            assert_eq!(RunLengths::from(coverage.clone()).0, runs);
            assert_eq!(Coverage::from(RunLengths(runs.to_vec())), coverage);

            let max = 5 + set.iter().copied().max().unwrap_or_default();
            for c in 0..max {
                assert_eq!(set.contains(&c), coverage.contains(c));
            }
        }

        test(&[], &[]);
        test(&[0], &[0, 1]);
        test(&[1], &[1, 1]);
        test(&[0, 1], &[0, 2]);
        test(&[0, 1, 3], &[0, 2, 1, 1]);
        test(
            // {2, 3, 4, 9, 10, 11, 15, 18, 19}
            &[18, 19, 2, 4, 9, 11, 15, 3, 3, 10],
            &[2, 3, 4, 3, 3, 1, 2, 2],
        );
    }

    #[test]
    fn test_coverage_serialization() {
        // The serialized form is the documented run-length encoding.
        let coverage = Coverage::from_vec(vec![2, 3, 4, 9, 10, 11, 15, 18, 19]);
        let json = serde_json::to_string(&coverage).unwrap();
        assert_eq!(json, "[2,3,4,3,3,1,2,2]");
        assert_eq!(serde_json::from_str::<Coverage>(&json).unwrap(), coverage);
    }

    #[test]
    fn test_coverage_iter() {
        let codepoints = vec![2, 3, 7, 8, 9, 14, 15, 19, 21];
        let coverage = Coverage::from_vec(codepoints.clone());
        assert_eq!(coverage.iter().collect::<Vec<_>>(), codepoints);
    }

    #[test]
    fn test_coverage_union() {
        let right = Coverage::from_vec(vec![2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 14]);
        let left =
            Coverage::from_vec(vec![0, 1, 2, 3, 5, 6, 9, 10, 11, 12, 13, 16, 17, 18]);

        let combined = right.union(&left);

        let codepoints = combined.iter().collect::<Vec<_>>();
        assert_eq!(
            codepoints,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 18]
        );
        assert_eq!(RunLengths::from(combined).0, vec![0, 15, 1, 3]);
    }

    #[test]
    fn test_cmap_ranges_match_codepoints() {
        let mut checked = 0;
        for data in typst_dev_assets::fonts() {
            let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
            for index in 0..count {
                let Ok(ttf) = ttf_parser::Face::parse(data, index) else { continue };
                let Some(cmap) = ttf.tables().cmap else { continue };
                let raw = ttf.raw_face().table(ttf_parser::Tag::from_bytes(b"cmap"));
                for (i, subtable) in cmap.subtables.into_iter().enumerate() {
                    let mut expected = CoverageBuilder::with_capacity(0);
                    subtable.codepoints(|c| expected.add_codepoint(c));
                    let mut actual = CoverageBuilder::with_capacity(0);
                    if cmap_ranges(raw.unwrap(), i, |r| actual.add_codepoint_range(r)) {
                        assert_eq!(actual.build().0, expected.build().0);
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 0);
    }
}
