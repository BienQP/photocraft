//! Swift Brush organising data: colour-coded groups (kept in the Brushes panel's view state, so
//! it serialises with the UI and the control channel can read and drive it) and the tag helpers.
//! Tags themselves live on the presets ([`photocraft_paint::BrushPreset::tags`]), so they follow
//! renames and are saved with the session. Clean-room: group colours and tag filtering are the common ideas of brush
//! organisers; nothing here comes from any plug-in.
//!
//! Filter syntax: words are ANDed; a word starting with `#` matches a tag (prefix,
//! case-insensitive), any other word matches the preset name (substring, case-insensitive).

use serde::{Deserialize, Serialize};

/// The colours a group can take (name, sRGB).
pub const PALETTE: [(&str, [u8; 3]); 8] = [
    ("Red", [219, 84, 84]),
    ("Orange", [226, 142, 62]),
    ("Yellow", [214, 190, 70]),
    ("Green", [96, 180, 96]),
    ("Teal", [64, 170, 170]),
    ("Blue", [84, 134, 224]),
    ("Purple", [150, 100, 210]),
    ("Pink", [214, 102, 160]),
];

pub use photocraft_paint::{MAX_TAG_LEN, MAX_TAGS, clean_tag};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SwiftBrush {
    /// Group key → colour.
    pub group_colors: Vec<(String, [u8; 3])>,
}

impl SwiftBrush {
    pub fn color(&self, group: &str) -> Option<[u8; 3]> {
        self.group_colors.iter().find(|(g, _)| g == group).map(|(_, c)| *c)
    }

    pub fn set_color(&mut self, group: &str, color: Option<[u8; 3]>) {
        self.group_colors.retain(|(g, _)| g != group);
        if let Some(c) = color {
            self.group_colors.push((group.to_string(), c));
        }
    }

    /// Every tag in use on `presets`, sorted, once each.
    pub fn all_tags(presets: &[photocraft_paint::BrushPreset]) -> Vec<String> {
        let mut v: Vec<String> = presets.iter().flat_map(|p| p.tags.iter().cloned()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Drops colours of groups that no longer exist.
    pub fn retain_known(&mut self, groups: &[&str]) {
        self.group_colors.retain(|(g, _)| groups.contains(&g.as_str()));
    }
}

/// `current` with `tag` added (`on`) or removed; `None` when nothing would change.
pub fn toggled(current: &[String], tag: &str, on: bool) -> Option<Vec<String>> {
    let tag = clean_tag(tag)?;
    let has = current.contains(&tag);
    if on == has || (on && current.len() >= MAX_TAGS) {
        return None;
    }
    let mut v: Vec<String> = current.iter().filter(|t| **t != tag).cloned().collect();
    if on {
        v.push(tag);
        v.sort();
    }
    Some(v)
}

/// Does a preset called `name` with `tags` pass `filter` (see the module docs)? An empty filter
/// passes everything.
pub fn matches(name: &str, tags: &[String], filter: &str) -> bool {
    let name = name.to_lowercase();
    filter.split_whitespace().all(|w| match w.strip_prefix('#') {
        Some("") => true,
        Some(t) => tags.iter().any(|x| x.starts_with(&t.to_lowercase())),
        None => name.contains(&w.to_lowercase()),
    })
}

/// The filter text after clicking tag chip `tag`: selects it, or clears it when it is the only
/// thing filtered.
pub fn toggle_tag_filter(filter: &str, tag: &str) -> String {
    let want = format!("#{tag}");
    let mut words: Vec<&str> = filter.split_whitespace().collect();
    match words.iter().position(|w| w.eq_ignore_ascii_case(&want)) {
        Some(i) => {
            words.remove(i);
        }
        None => words.push(&want),
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_cleaned_toggled_and_bounded() {
        assert_eq!(clean_tag("  #Ink Wash "), Some("ink-wash".into()));
        assert_eq!(clean_tag("###"), None);
        assert_eq!(clean_tag(""), None);
        assert_eq!(clean_tag(&"x".repeat(100)).map(|t| t.len()), Some(MAX_TAG_LEN));
        let mut tags: Vec<String> = Vec::new();
        tags = toggled(&tags, "Dry", true).unwrap_or(tags);
        assert_eq!(toggled(&tags, "dry", true), None, "no duplicates");
        tags = toggled(&tags, "texture", true).unwrap();
        assert_eq!(tags, ["dry", "texture"]);
        tags = toggled(&tags, "DRY", false).unwrap();
        assert_eq!(toggled(&tags, "dry", false), None, "already gone");
        assert_eq!(toggled(&tags, "texture", false), Some(vec![]));
        let full: Vec<String> = (0..MAX_TAGS).map(|i| format!("t{i:02}")).collect();
        assert_eq!(toggled(&full, "one-more", true), None, "bounded");
    }

    #[test]
    fn group_colours_set_replace_and_clear() {
        let mut s = SwiftBrush::default();
        assert_eq!(s.color("Inks"), None);
        s.set_color("Inks", Some(PALETTE[0].1));
        s.set_color("Inks", Some(PALETTE[5].1));
        assert_eq!((s.color("Inks"), s.group_colors.len()), (Some(PALETTE[5].1), 1));
        s.set_color("Inks", None);
        assert_eq!(s.color("Inks"), None);
    }

    #[test]
    fn filter_words_are_anded_and_hash_means_tag() {
        let tags = vec!["dry".to_string(), "texture".to_string()];
        assert!(matches("Chalk", &tags, ""));
        assert!(matches("Chalk", &tags, "cha"));
        assert!(matches("Chalk", &tags, "#dry"));
        assert!(matches("Chalk", &tags, "#tex"), "tag prefix");
        assert!(matches("Chalk", &tags, "cha #dry"));
        assert!(!matches("Chalk", &tags, "#wet"));
        assert!(!matches("Chalk", &tags, "ink #dry"));
        assert!(!matches("Chalk", &[], "#dry"));
        assert!(matches("Chalk", &[], "#"), "a bare # filters nothing yet");
    }

    #[test]
    fn clicking_a_chip_toggles_its_word_in_the_filter() {
        assert_eq!(toggle_tag_filter("", "dry"), "#dry");
        assert_eq!(toggle_tag_filter("#dry", "dry"), "");
        assert_eq!(toggle_tag_filter("ink #Dry", "dry"), "ink");
        assert_eq!(toggle_tag_filter("ink", "dry"), "ink #dry");
    }

    #[test]
    fn retain_known_prunes_gone_groups() {
        let mut s = SwiftBrush::default();
        s.set_color("Inks", Some([1, 2, 3]));
        s.set_color("Old", Some([1, 2, 3]));
        s.retain_known(&["Inks"]);
        assert_eq!(s.group_colors.len(), 1);
    }
}
