package internal

import (
	"testing"
)

func TestParseKeySplitsNamespaceAndStripsDoubleHash(t *testing.T) {
	p := ParseKey("WpDcVyo5fP:product-detail-route-1195e812ffd3266c2ed02a5d603d26f5-481ce4e8cb0b80a09beac0aaeea37457", 10)
	if p.Namespace != "WpDcVyo5fP" {
		t.Errorf("namespace: got %q, want %q", p.Namespace, "WpDcVyo5fP")
	}
	if p.KeyType != "product-detail-route" {
		t.Errorf("key_type: got %q, want %q", p.KeyType, "product-detail-route")
	}
}

func TestParseKeyStripsSingleHash(t *testing.T) {
	p := ParseKey("WpDcVyo5fP:faq-group-930338b2525f792b15a15f598e3ade05", 10)
	if p.KeyType != "faq-group" {
		t.Errorf("key_type: got %q, want %q", p.KeyType, "faq-group")
	}
}

func TestParseKeyHandlesTripleHash(t *testing.T) {
	p := ParseKey("WpDcVyo5fP:cached-product-df61c92879d105261d7fc1e860f1acc7-742e3c7d1b07ae59066c16c7d3f35d4d-450e84ef6ea42a6d10b09c1b4c84ec89", 10)
	if p.KeyType != "cached-product" {
		t.Errorf("key_type: got %q, want %q", p.KeyType, "cached-product")
	}
}

func TestParseKeyPreservesTagPrefix(t *testing.T) {
	p := ParseKey("WpDcVyo5fP:\x01tags\x01product-0f483ff8ad7735e5e2dc0f44728d7bdb", 10)
	if p.Namespace != "WpDcVyo5fP" {
		t.Errorf("namespace: got %q", p.Namespace)
	}
	if p.KeyType != "\\x01tags\\x01product" {
		t.Errorf("key_type: got %q, want %q", p.KeyType, "\\x01tags\\x01product")
	}
}

func TestParseKeyPreservesAlreadyEscapedTagPrefix(t *testing.T) {
	p := ParseKey("WpDcVyo5fP:\\x01tags\\x01product-0f483ff8ad7735e5e2dc0f44728d7bdb", 10)
	if p.KeyType != "\\x01tags\\x01product" {
		t.Errorf("key_type: got %q", p.KeyType)
	}
}

func TestParseKeyNoColonFallsBackToPrefix(t *testing.T) {
	p := ParseKey("plainkeywithoutcolon12345", 10)
	if p.Namespace != "plainkeywi" {
		t.Errorf("namespace: got %q, want %q", p.Namespace, "plainkeywi")
	}
	if p.KeyType != "(no-type)" {
		t.Errorf("key_type: got %q, want %q", p.KeyType, "(no-type)")
	}
}

func TestParseKeyShortHexWordNotTreatedAsHash(t *testing.T) {
	p := ParseKey("WpDcVyo5fP:cafe", 10)
	if p.KeyType != "cafe" {
		t.Errorf("short hex words should not be stripped: got %q", p.KeyType)
	}
}

func TestParseKeyBodyOnlyHash(t *testing.T) {
	p := ParseKey("WpDcVyo5fP:0f483ff8ad7735e5e2dc0f44728d7bdb", 10)
	if p.KeyType != "(hash)" {
		t.Errorf("body-only hash: got %q, want '(hash)'", p.KeyType)
	}
}

func TestDisplayTypeHumanizesTagSets(t *testing.T) {
	got := DisplayType("\\x01tags\\x01product")
	if got != "🏷 product  [tag]" {
		t.Errorf("DisplayType: got %q", got)
	}
}

func TestDisplayTypePassesThroughNormal(t *testing.T) {
	got := DisplayType("product-detail-route")
	if got != "product-detail-route" {
		t.Errorf("DisplayType: got %q", got)
	}
}

func TestDisplayKeyRewritesTagMarker(t *testing.T) {
	got := DisplayKey("WpDcVyo5fP:\\x01tags\\x01product-0f483ff8")
	if got != "WpDcVyo5fP:🏷 tags » product-0f483ff8" {
		t.Errorf("DisplayKey: got %q", got)
	}
}

func TestDisplayKeyPassesThroughNormal(t *testing.T) {
	got := DisplayKey("WpDcVyo5fP:product-1")
	if got != "WpDcVyo5fP:product-1" {
		t.Errorf("DisplayKey: got %q", got)
	}
}

func TestMatchPatternFor(t *testing.T) {
	tests := []struct {
		ns, keyType, want string
	}{
		{"WpDcVyo5fP", "product-detail-route", "WpDcVyo5fP:product-detail-route*"},
		{"ns", "\\x01tags\\x01product", "ns:\x01tags\x01product*"},
		{"ns", "(hash)", "ns:*"},
		{"ns", "(no-type)", "ns:*"},
	}
	for _, tc := range tests {
		got := MatchPatternFor(tc.ns, tc.keyType)
		if got != tc.want {
			t.Errorf("MatchPatternFor(%q, %q): got %q, want %q", tc.ns, tc.keyType, got, tc.want)
		}
	}
}

func TestEscapeUnescapeRoundtrip(t *testing.T) {
	raw := []byte("ns:\x01tags\x01product")
	esc := EscapeKey(raw)
	if esc != "ns:\\x01tags\\x01product" {
		t.Errorf("EscapeKey: got %q", esc)
	}
	unesc := UnescapeKey(esc)
	if string(unesc) != string(raw) {
		t.Errorf("UnescapeKey: got %q, want %q", unesc, raw)
	}
}
