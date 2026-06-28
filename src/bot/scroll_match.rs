use crate::aux::inventory::Item;

const HOME_SCROLL_ALIASES: &[&str] = &[
    "\u{50B3}\u{9001}\u{56DE}\u{5BB6}\u{7684}\u{5377}\u{8EF8}",
    "\u{50B3}\u{9001}\u{56DE}\u{5BB6}\u{5377}\u{8EF8}",
    "\u{4F20}\u{9001}\u{56DE}\u{5BB6}\u{7684}\u{5377}\u{8F74}",
    "\u{4F20}\u{9001}\u{56DE}\u{5BB6}\u{5377}\u{8F74}",
    "\u{56DE}\u{5BB6}\u{5377}\u{8EF8}",
    "\u{56DE}\u{5BB6}\u{5377}\u{8F74}",
];

const RANDOM_SCROLL_ALIASES: &[&str] = &[
    "\u{77AC}\u{9593}\u{79FB}\u{52D5}\u{5377}\u{8EF8}",
    "\u{77AC}\u{9593}\u{79FB}\u{52D5}\u{7684}\u{5377}\u{8EF8}",
    "\u{77AC}\u{95F4}\u{79FB}\u{52A8}\u{5377}\u{8F74}",
    "\u{77AC}\u{95F4}\u{79FB}\u{52A8}\u{7684}\u{5377}\u{8F74}",
    "\u{77AC}\u{79FB}",
];

pub(crate) fn teleport_scroll_item_matches(item: &Item, name_keyword: &str) -> bool {
    let item_name = item.name_lossy();
    teleport_scroll_name_matches(&item_name, name_keyword)
        || teleport_scroll_raw_name_matches(&item.name_raw, name_keyword)
}

pub(crate) fn teleport_scroll_name_matches(item_name: &str, name_keyword: &str) -> bool {
    let trimmed = name_keyword.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some(aliases) = scroll_aliases_for_keyword(trimmed) {
        return contains_any(item_name, aliases);
    }
    item_name.contains(trimmed)
}

fn teleport_scroll_raw_name_matches(name_raw: &[u8], name_keyword: &str) -> bool {
    let trimmed = name_keyword.trim();
    if trimmed.is_empty() {
        return false;
    }
    let raw = bytes_before_nul(name_raw);
    if raw.is_empty() {
        return false;
    }
    if let Some(aliases) = scroll_aliases_for_keyword(trimmed) {
        return aliases.iter().any(|alias| raw_contains_encoded(raw, alias));
    }
    raw_contains_encoded(raw, trimmed)
}

fn scroll_aliases_for_keyword(keyword: &str) -> Option<&'static [&'static str]> {
    if contains_any(keyword, HOME_SCROLL_ALIASES) {
        Some(HOME_SCROLL_ALIASES)
    } else if contains_any(keyword, RANDOM_SCROLL_ALIASES) {
        Some(RANDOM_SCROLL_ALIASES)
    } else {
        None
    }
}

fn contains_any(value: &str, aliases: &[&str]) -> bool {
    aliases.iter().any(|alias| value.contains(alias))
}

fn bytes_before_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..end]
}

fn raw_contains_encoded(raw: &[u8], text: &str) -> bool {
    raw_contains(raw, text.as_bytes())
        || encoded_without_errors(text, encoding_rs::BIG5).is_some_and(|b| raw_contains(raw, &b))
        || encoded_without_errors(text, encoding_rs::GBK).is_some_and(|b| raw_contains(raw, &b))
}

fn encoded_without_errors(text: &str, encoding: &'static encoding_rs::Encoding) -> Option<Vec<u8>> {
    let (bytes, _, had_errors) = encoding.encode(text);
    (!had_errors).then(|| bytes.into_owned())
}

fn raw_contains(raw: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && raw.windows(needle.len()).any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_with_raw_name(raw: Vec<u8>) -> Item {
        Item {
            entry_addr: 0,
            item_param: 0,
            item_type: 0,
            icon: 0,
            equipped: false,
            count: 1,
            name_raw: raw,
        }
    }

    #[test]
    fn string_matching_accepts_traditional_config_and_simplified_inventory_name() {
        assert!(teleport_scroll_name_matches(
            "\u{77AC}\u{95F4}\u{79FB}\u{52A8}\u{5377}\u{8F74}",
            "\u{77AC}\u{9593}\u{79FB}\u{52D5}\u{5377}\u{8EF8}",
        ));
        assert!(teleport_scroll_name_matches(
            "\u{4F20}\u{9001}\u{56DE}\u{5BB6}\u{5377}\u{8F74}",
            "\u{50B3}\u{9001}\u{56DE}\u{5BB6}\u{5377}\u{8EF8}",
        ));
    }

    #[test]
    fn raw_gbk_scroll_name_matches_traditional_keyword() {
        let (raw, _, had_errors) =
            encoding_rs::GBK.encode("\u{77AC}\u{95F4}\u{79FB}\u{52A8}\u{5377}\u{8F74} (10)");
        assert!(!had_errors);

        assert!(teleport_scroll_raw_name_matches(
            &raw,
            "\u{77AC}\u{9593}\u{79FB}\u{52D5}\u{5377}\u{8EF8}",
        ));
    }

    #[test]
    fn raw_gbk_home_scroll_name_matches_traditional_keyword() {
        let (raw, _, had_errors) =
            encoding_rs::GBK.encode("\u{4F20}\u{9001}\u{56DE}\u{5BB6}\u{5377}\u{8F74}");
        assert!(!had_errors);

        assert!(teleport_scroll_raw_name_matches(
            &raw,
            "\u{50B3}\u{9001}\u{56DE}\u{5BB6}\u{5377}\u{8EF8}",
        ));
    }

    #[test]
    fn item_match_uses_raw_name_when_decoded_name_does_not_match_keyword() {
        let mut item = item_with_raw_name(b"garbled ".to_vec());
        let (raw, _, had_errors) =
            encoding_rs::GBK.encode("\u{77AC}\u{95F4}\u{79FB}\u{52A8}\u{5377}\u{8F74}");
        assert!(!had_errors);
        item.name_raw = raw.into_owned();

        assert!(teleport_scroll_item_matches(
            &item,
            "\u{77AC}\u{9593}\u{79FB}\u{52D5}\u{5377}\u{8EF8}",
        ));
    }
}
