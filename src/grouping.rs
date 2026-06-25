//! Key parsing: split a Redis key into a `namespace` and a normalized `type`.
//!
//! Shopware (and the Symfony cache it wraps) produce keys shaped like:
//!
//! ```text
//! WpDcVyo5fP:product-detail-route-1195e812ffd3266c2ed02a5d603d26f5-481ce4e8cb0b80a09beac0aaeea37457
//! WpDcVyo5fP:\x01tags\x01product-0f483ff8ad7735e5e2dc0f44728d7bdb
//! WpDcVyo5fP:cached-product-df61c92879d105261d7fc1e860f1acc7-742e3c7d1b07ae59066c16c7d3f35d4d-450e84ef6ea42a6d10b09c1b4c84ec89
//! ```
//!
//! The first colon-delimited segment is the *namespace* (a per-install random
//! prefix). What follows identifies the cache *type*; we normalize it by
//! stripping the trailing hash segments so that the thousands of individual
//! product caches collapse into one `product-detail-route` bucket.

/// Result of parsing a single key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKey {
    pub namespace: String,
    pub key_type: String,
}

/// Returns true if `s` looks like a hex hash segment (md5/sha-ish): all hex
/// digits and reasonably long. We require length >= 16 to avoid eating short
/// meaningful words that happen to be hex (e.g. "face", "dead").
fn is_hash_segment(s: &str) -> bool {
    s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Split a raw key into namespace + normalized type.
///
/// `namespace_len` is how many leading bytes of the key are treated as the
/// namespace when there is no colon (mirrors the PHP "first N chars" fallback).
pub fn parse_key(key: &str, namespace_len: usize) -> ParsedKey {
    // The Symfony tag marker is the byte 0x01. Render it as the readable token
    // "\x01" (already-escaped input passes through unchanged, since it has no
    // literal 0x01 byte to replace).
    let normalized = key.replace('\u{0001}', "\\x01");

    // Namespace = text before the first ':'. Fall back to first N chars.
    let (namespace, body) = match normalized.find(':') {
        Some(idx) => (normalized[..idx].to_string(), &normalized[idx + 1..]),
        None => {
            let n = namespace_len.min(normalized.len());
            (normalized[..n].to_string(), "")
        }
    };

    let key_type = normalize_type(body);
    ParsedKey {
        namespace,
        key_type,
    }
}

/// Strip trailing hash segments from the key body to derive a stable type name.
///
/// `product-detail-route-<hash>-<hash>` -> `product-detail-route`
/// `\x01tags\x01product-<hash>`         -> `\x01tags\x01product`
fn normalize_type(body: &str) -> String {
    if body.is_empty() {
        return "(no-type)".to_string();
    }

    // Preserve the \x01tags\x01 prefix verbatim, normalize only the remainder.
    const TAG_PREFIX: &str = "\\x01tags\\x01";
    let (prefix, rest) = if let Some(stripped) = body.strip_prefix(TAG_PREFIX) {
        (TAG_PREFIX, stripped)
    } else {
        ("", body)
    };

    let mut segments: Vec<&str> = rest.split('-').collect();

    // Pop trailing hash-looking segments, but always keep at least the first
    // segment so we never collapse everything to an empty string.
    while segments.len() > 1 && is_hash_segment(segments.last().unwrap()) {
        segments.pop();
    }

    // Edge case: a body that is *only* a hash (no descriptive prefix) — keep a
    // generic bucket rather than the raw hash, which would explode cardinality.
    if segments.len() == 1 && is_hash_segment(segments[0]) {
        return format!("{prefix}(hash)");
    }

    format!("{prefix}{}", segments.join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(key: &str) -> ParsedKey {
        parse_key(key, 10)
    }

    #[test]
    fn splits_namespace_and_strips_double_hash() {
        let p = t("WpDcVyo5fP:product-detail-route-1195e812ffd3266c2ed02a5d603d26f5-481ce4e8cb0b80a09beac0aaeea37457");
        assert_eq!(p.namespace, "WpDcVyo5fP");
        assert_eq!(p.key_type, "product-detail-route");
    }

    #[test]
    fn strips_single_hash() {
        let p = t("WpDcVyo5fP:faq-group-930338b2525f792b15a15f598e3ade05");
        assert_eq!(p.key_type, "faq-group");
    }

    #[test]
    fn handles_triple_hash() {
        let p = t("WpDcVyo5fP:cached-product-df61c92879d105261d7fc1e860f1acc7-742e3c7d1b07ae59066c16c7d3f35d4d-450e84ef6ea42a6d10b09c1b4c84ec89");
        assert_eq!(p.key_type, "cached-product");
    }

    #[test]
    fn preserves_tag_prefix() {
        // literal control byte form
        let p = t("WpDcVyo5fP:\u{0001}tags\u{0001}product-0f483ff8ad7735e5e2dc0f44728d7bdb");
        assert_eq!(p.namespace, "WpDcVyo5fP");
        assert_eq!(p.key_type, "\\x01tags\\x01product");
    }

    #[test]
    fn preserves_already_escaped_tag_prefix() {
        let p = t("WpDcVyo5fP:\\x01tags\\x01product-0f483ff8ad7735e5e2dc0f44728d7bdb");
        assert_eq!(p.key_type, "\\x01tags\\x01product");
    }

    #[test]
    fn review_route_single_hash() {
        let p = t("WpDcVyo5fP:product-review-route-edc0e4a2cce33d9d8122d93bbe90a6f0-58f97500f374ea9a13ace9f52c7491b3");
        assert_eq!(p.key_type, "product-review-route");
    }

    #[test]
    fn no_colon_falls_back_to_prefix() {
        let p = t("plainkeywithoutcolon12345");
        assert_eq!(p.namespace, "plainkeywi");
        assert_eq!(p.key_type, "(no-type)");
    }

    #[test]
    fn short_hex_word_is_not_treated_as_hash() {
        // "cafe" is hex but too short — must be kept as a real type token.
        let p = t("WpDcVyo5fP:cafe");
        assert_eq!(p.key_type, "cafe");
    }

    #[test]
    fn body_that_is_only_a_hash() {
        let p = t("WpDcVyo5fP:0f483ff8ad7735e5e2dc0f44728d7bdb");
        assert_eq!(p.key_type, "(hash)");
    }
}
