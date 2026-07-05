package internal

import (
	"net/url"
	"strings"
)

const tagMarker = "\x01tags\x01"
const escapedTagMarker = "\\x01tags\\x01"

// ParsedKey is the result of parsing a single key.
type ParsedKey struct {
	Namespace string
	KeyType   string
}

// isHashSegment returns true if s looks like a hex hash segment (md5/sha-ish).
func isHashSegment(s string) bool {
	if len(s) < 16 {
		return false
	}
	for _, r := range s {
		if !isHexDigit(r) {
			return false
		}
	}
	return true
}

func isHexDigit(r rune) bool {
	return (r >= '0' && r <= '9') || (r >= 'a' && r <= 'f') || (r >= 'A' && r <= 'F')
}

// ParseKey splits a raw key into namespace + normalized type.
// namespaceLen is how many leading bytes of the key are treated as the
// namespace when there is no colon.
func ParseKey(key string, namespaceLen int) ParsedKey {
	// Render the 0x01 byte as the readable token "\x01".
	normalized := strings.ReplaceAll(key, tagMarker, escapedTagMarker)

	// Namespace = text before the first ':'. Fall back to first N chars.
	var namespace, body string
	if idx := strings.Index(normalized, ":"); idx >= 0 {
		namespace = normalized[:idx]
		body = normalized[idx+1:]
	} else {
		n := namespaceLen
		if n > len(normalized) {
			n = len(normalized)
		}
		namespace = normalized[:n]
		body = ""
	}

	keyType := normalizeType(body)
	return ParsedKey{Namespace: namespace, KeyType: keyType}
}

// normalizeType strips trailing hash segments from the key body to derive a stable type name.
func normalizeType(body string) string {
	if body == "" {
		return "(no-type)"
	}

	// Preserve the \x01tags\x01 prefix verbatim, normalize only the remainder.
	var prefix, rest string
	if strings.HasPrefix(body, escapedTagMarker) {
		prefix = escapedTagMarker
		rest = body[len(escapedTagMarker):]
	} else {
		prefix = ""
		rest = body
	}

	segments := strings.Split(rest, "-")

	// Pop trailing hash-looking segments, always keeping at least the first.
	for len(segments) > 1 && isHashSegment(segments[len(segments)-1]) {
		segments = segments[:len(segments)-1]
	}

	// Edge case: body that is *only* a hash.
	if len(segments) == 1 && isHashSegment(segments[0]) {
		return prefix + "(hash)"
	}

	return prefix + strings.Join(segments, "-")
}

// IsTagType returns true if the type name represents a Symfony tag set.
func IsTagType(name string) bool {
	return strings.Contains(name, escapedTagMarker)
}

// DisplayType humanizes a type name for the TUI. The Symfony tag-index set
// is shown with a visible badge so it stands out regardless of terminal emoji support.
func DisplayType(name string) string {
	if strings.Contains(name, escapedTagMarker) {
		tag := strings.ReplaceAll(name, escapedTagMarker, "")
		if unescaped, err := url.QueryUnescape(tag); err == nil && unescaped != tag {
			tag = unescaped
		}
		return "🏷 " + tag + "  [tag]"
	}
	if unescaped, err := url.QueryUnescape(name); err == nil && unescaped != name {
		return unescaped
	}
	return name
}

// DisplayNamespace humanizes a namespace for the TUI.
func DisplayNamespace(ns string) string {
	if unescaped, err := url.QueryUnescape(ns); err == nil && unescaped != ns {
		return unescaped
	}
	return ns
}

// DisplayKey humanizes a full key for the TUI (biggest-keys view).
func DisplayKey(key string) string {
	idx := strings.Index(key, escapedTagMarker)
	if idx >= 0 {
		before := key[:idx]
		tag := key[idx+len(escapedTagMarker):]
		
		if tag == "" {
			colonIdx := strings.Index(before, ":")
			if colonIdx >= 0 {
				tagName := before[colonIdx+1:]
				return before[:colonIdx+1] + "🏷 tags » " + tagName
			}
			return "🏷 tags » " + before
		}
		
		return before + "🏷 tags » " + tag
	}
	if unescaped, err := url.QueryUnescape(key); err == nil && unescaped != key {
		return unescaped
	}
	return key
}

// EscapeKey renders control bytes in a key name readably.
func EscapeKey(bytes []byte) string {
	return strings.ReplaceAll(string(bytes), tagMarker, escapedTagMarker)
}

// UnescapeKey restores control bytes from escaped form.
func UnescapeKey(key string) []byte {
	return []byte(strings.ReplaceAll(key, escapedTagMarker, tagMarker))
}

// MatchPatternFor builds the SCAN MATCH pattern for a (namespace, type).
func MatchPatternFor(namespace, keyType string) string {
	body := strings.ReplaceAll(keyType, escapedTagMarker, tagMarker)
	if body == "(no-type)" {
		return namespace + ":*"
	}
	if strings.HasSuffix(body, "(hash)") {
		stripped := strings.TrimSuffix(body, "(hash)")
		return namespace + ":" + stripped + "*"
	}
	return namespace + ":" + body + "*"
}
