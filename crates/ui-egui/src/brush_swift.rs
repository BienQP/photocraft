//! Swift Brush organising data: colour-coded groups and tags on brush presets, kept in the
//! Brushes panel's view state (so it serialises with the UI and the control channel can read and
//! drive it). Clean-room: group colours and tag filtering are the common ideas of brush
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

/// Most tags per preset and longest tag, so hostile files or scripts can't bloat the state.
pub const MAX_TAGS: usize = 16;
pub const MAX_TAG_LEN: usize = 24;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SwiftBrush {
    /// Group key → colour.
    pub group_colors: Vec<(String, [u8; 3])>,
    /// Preset name → its tags.
    pub tags: Vec<(String, Vec<String>)>,
}

/// A tag as stored: trimmed, no leading `#`, no whitespace inside, lower-case, bounded.
pub fn clean_tag(raw: &str) -> Option<String> {
    let t: String = raw.trim().trim_start_matches('#').split_whitespace().collect::<Vec<_>>().join("-").to_lowercase();
    let t: String = t.chars().take(MAX_TAG_LEN).collect();
    (!t.is_empty()).then_some(t)
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

    pub fn tags_of(&self, preset: &str) -> &[String] {
        self.tags.iter().find(|(n, _)| n == preset).map_or(&[], |(_, t)| t.as_slice())
    }

    /// Adds (`on`) or removes `tag` on `preset`; false when nothing changed.
    pub fn set_tag(&mut self, preset: &str, tag: &str, on: bool) -> bool {
        let Some(tag) = clean_tag(tag) else { return false };
        let i = self.tags.iter().position(|(n, _)| n == preset);
        if on {
            match i {
                Some(i) => {
                    let list = &mut self.tags[i].1;
                    if list.contains(&tag) || list.len() >= MAX_TAGS {
                        return false;
                    }
                    list.push(tag);
                    list.sort();
                }
                None => self.tags.push((preset.to_string(), vec![tag])),
            }
            true
        } else {
            let Some(i) = i else { return false };
            let before = self.tags[i].1.len();
            self.tags[i].1.retain(|t| *t != tag);
            let changed = self.tags[i].1.len() != before;
            if self.tags[i].1.is_empty() {
                self.tags.remove(i);
            }
            changed
        }
    }

    /// Every tag in use, sorted, once each.
    pub fn all_tags(&self) -> Vec<String> {
        let mut v: Vec<String> = self.tags.iter().flat_map(|(_, t)| t.iter().cloned()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Drops tags and colours of presets and groups that no longer exist.
    pub fn retain_known(&mut self, presets: &[&str], groups: &[&str]) {
        self.tags.retain(|(n, _)| presets.contains(&n.as_str()));
        self.group_colors.retain(|(g, _)| groups.contains(&g.as_str()));
    }
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
    fn tags_are_cleaned_added_removed_and_bounded() {
        assert_eq!(clean_tag("  #Ink Wash "), Some("ink-wash".into()));
        assert_eq!(clean_tag("###"), None);
        assert_eq!(clean_tag(""), None);
        assert_eq!(clean_tag(&"x".repeat(100)).map(|t| t.len()), Some(MAX_TAG_LEN));
        let mut s = SwiftBrush::default();
        assert!(s.set_tag("Chalk", "Dry", true));
        assert!(!s.set_tag("Chalk", "dry", true), "no duplicates");
        assert!(s.set_tag("Chalk", "texture", true));
        assert_eq!(s.tags_of("Chalk"), ["dry", "texture"]);
        assert!(s.set_tag("Chalk", "DRY", false));
        assert!(!s.set_tag("Chalk", "dry", false), "already gone");
        assert!(!s.set_tag("Nope", "x", false));
        assert!(s.set_tag("Chalk", "texture", false));
        assert!(s.tags.is_empty(), "an empty list is dropped");
        for i in 0..40 {
            s.set_tag("Many", &format!("t{i}"), true);
        }
        assert_eq!(s.tags_of("Many").len(), MAX_TAGS);
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
    fn retain_known_prunes_the_gone() {
        let mut s = SwiftBrush::default();
        s.set_tag("Chalk", "a", true);
        s.set_tag("Gone", "a", true);
        s.set_color("Inks", Some([1, 2, 3]));
        s.set_color("Old", Some([1, 2, 3]));
        s.retain_known(&["Chalk"], &["Inks"]);
        assert_eq!((s.tags.len(), s.group_colors.len()), (1, 1));
    }
}
