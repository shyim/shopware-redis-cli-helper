package tui

import (
	"strings"
	"testing"

	"github.com/shopware-redis-cli-helper/internal"
)

// TestBox verifies the box helper renders borders correctly.
func TestBox(t *testing.T) {
	b := box("Title", true, 30, 5, "hello")
	lines := strings.Split(b, "\n")
	if len(lines) != 5 {
		t.Errorf("expected 5 lines, got %d", len(lines))
	}
	// First line should contain "Title"
	if !strings.Contains(lines[0], "Title") {
		t.Errorf("title not in top border: %q", lines[0])
	}
	// Should contain "hello" in content
	found := false
	for _, l := range lines {
		if strings.Contains(l, "hello") {
			found = true
		}
	}
	if !found {
		t.Errorf("content 'hello' not found in box")
	}
}

// TestBoxSmall ensures we don't crash on tiny dimensions.
func TestBoxSmall(t *testing.T) {
	b := box("X", true, 4, 3, "hi")
	if b == "" {
		t.Error("box should not be empty")
	}
}

// TestTrunc ensures truncation works with display width.
func TestTrunc(t *testing.T) {
	if trunc("hello", 10) != "hello" {
		t.Error("short string should pass through")
	}
	if trunc("hello world", 5) != "hell…" {
		t.Errorf("trunc: got %q", trunc("hello world", 5))
	}
}

// TestPadRight ensures padding adds correct number of spaces.
func TestPadRight(t *testing.T) {
	s := padRight("hi", 5)
	if s != "hi   " {
		t.Errorf("padRight: got %q", s)
	}
}

// TestRenderPicker verifies the picker screen renders.
func TestRenderPicker(t *testing.T) {
	m := NewModel("redis://localhost:6379", 5, "", internal.ScanOptions{Count: 1000}, nil)
	m.width = 120
	m.height = 40
	m.screen = ScreenPicking

	out := m.render()
	if !strings.Contains(out, "Shopware Redis Insights") {
		t.Error("picker missing title")
	}
	if !strings.Contains(out, "Basic") {
		t.Error("picker missing Basic option")
	}
	if !strings.Contains(out, "Advanced") {
		t.Error("picker missing Advanced option")
	}
}

// TestRenderBrowse verifies the browse view renders with sample data.
func TestRenderBrowse(t *testing.T) {
	m := NewModel("redis://localhost:6379", 5, "", internal.ScanOptions{Count: 1000, Biggest: 10}, nil)
	m.width = 120
	m.height = 40
	m.screen = ScreenRunning
	m.advanced = true
	m.done = true
	m.totalScanned = 5

	// Add sample data
	b := int64(100)
	dt := "string"
	ttl := int64(-1)
	m.stats.Record("alpha:product-1", "alpha", "product", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})
	m.stats.Record("alpha:product-2", "alpha", "product", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})
	m.stats.Record("beta:cart-1", "beta", "cart", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})

	out := m.render()
	if !strings.Contains(out, "alpha") {
		t.Error("browse missing namespace 'alpha'")
	}
	if !strings.Contains(out, "product") {
		t.Error("browse missing type 'product'")
	}
	if !strings.Contains(out, "Namespaces") {
		t.Error("browse missing Namespaces title")
	}
}

// TestRenderBiggest verifies biggest keys view.
func TestRenderBiggest(t *testing.T) {
	m := NewModel("redis://localhost:6379", 5, "", internal.ScanOptions{Count: 1000, Biggest: 10}, nil)
	m.width = 120
	m.height = 40
	m.screen = ScreenRunning
	m.advanced = true
	m.done = true

	b := int64(5000)
	dt := "string"
	ttl := int64(-1)
	m.stats.Record("alpha:review-1", "alpha", "review", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})

	m.view = ViewBiggest
	out := m.render()
	if !strings.Contains(out, "Biggest") {
		t.Error("biggest view missing title")
	}
	if !strings.Contains(out, "review-1") {
		t.Error("biggest view missing key")
	}
}

// TestRenderPersistent verifies persistent keys view.
func TestRenderPersistent(t *testing.T) {
	m := NewModel("redis://localhost:6379", 5, "", internal.ScanOptions{Count: 1000}, nil)
	m.width = 120
	m.height = 40
	m.screen = ScreenRunning
	m.advanced = true
	m.done = true

	b := int64(1000)
	dt := "string"
	ttl := int64(-1)
	m.stats.Record("ns:config-1", "ns", "config", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})

	m.view = ViewPersistent
	out := m.render()
	if !strings.Contains(out, "persistent") {
		t.Error("persistent view missing 'persistent'")
	}
	if !strings.Contains(out, "config") {
		t.Error("persistent view missing type")
	}
}

// TestRenderTagBadge verifies tag types show the TAG badge.
func TestRenderTagBadge(t *testing.T) {
	m := NewModel("redis://localhost:6379", 5, "", internal.ScanOptions{Count: 1000}, nil)
	m.width = 120
	m.height = 40
	m.screen = ScreenRunning
	m.advanced = true
	m.done = true

	b := int64(100)
	dt := "set"
	ttl := int64(-1)
	m.stats.Record("ns:\\x01tags\\x01product", "ns", "\\x01tags\\x01product", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})

	out := m.render()
	if !strings.Contains(out, "TAG") {
		t.Error("tag badge missing in browse view")
	}
}

// TestVisualDump renders the full TUI and prints it (for manual inspection).
func TestVisualDump(t *testing.T) {
	m := NewModel("redis://localhost:6379", 5, "", internal.ScanOptions{Count: 1000, Biggest: 10, WantMemory: true, WantTypes: true, WantTTL: true}, nil)
	m.width = 120
	m.height = 40
	m.screen = ScreenRunning
	m.advanced = true
	m.done = true
	m.totalScanned = 5

	b := int64(100)
	largeB := int64(5000)
	dt := "string"
	ttl := int64(-1)
	m.stats.Record("alpha:product-1", "alpha", "product", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})
	m.stats.Record("alpha:product-2", "alpha", "product", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})
	m.stats.Record("alpha:review-1", "alpha", "review", &internal.KeyObservation{Bytes: &largeB, DataType: &dt, TTL: &ttl})
	m.stats.Record("beta:cart-1", "beta", "cart", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})
	m.stats.Record("ns:\\x01tags\\x01product", "ns", "\\x01tags\\x01product", &internal.KeyObservation{Bytes: &b, DataType: &dt, TTL: &ttl})

	// Select the 'ns' namespace to see tag types
	m.nsCursor = 2 // ns is 3rd alphabetically

	t.Logf("\n%s", m.render())
}
