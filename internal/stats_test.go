package internal

import "testing"

func obs(bytes int64, ttl int64) *KeyObservation {
	b := bytes
	dt := "string"
	t := ttl
	return &KeyObservation{
		Bytes:    &b,
		DataType: &dt,
		TTL:      &t,
	}
}

func TestStatsRecordAndAggregate(t *testing.T) {
	s := NewStats(0)
	s.Record("ns1:a", "ns1", "product", obs(100, -1))
	s.Record("ns1:b", "ns1", "product", obs(300, 60))

	if s.TotalKeys != 2 {
		t.Fatalf("total_keys: got %d, want 2", s.TotalKeys)
	}

	stat := s.Namespaces["ns1"]["product"]
	if stat.Count != 2 {
		t.Errorf("count: got %d, want 2", stat.Count)
	}
	if stat.TotalBytes != 400 {
		t.Errorf("total_bytes: got %d, want 400", stat.TotalBytes)
	}
	if stat.AvgBytes() != 200 {
		t.Errorf("avg_bytes: got %d, want 200", stat.AvgBytes())
	}
	if stat.Persistent != 1 {
		t.Errorf("persistent: got %d, want 1", stat.Persistent)
	}
	if stat.Expiring != 1 {
		t.Errorf("expiring: got %d, want 1", stat.Expiring)
	}
	if stat.PersistentBytes != 100 {
		t.Errorf("persistent_bytes: got %d, want 100", stat.PersistentBytes)
	}
	if stat.DataTypes["string"] != 2 {
		t.Errorf("data_types: got %d", stat.DataTypes["string"])
	}
	if s.NamespaceCount("ns1") != 2 {
		t.Errorf("namespace_count: got %d", s.NamespaceCount("ns1"))
	}
}

func TestBiggestKeysBoundedAndSorted(t *testing.T) {
	s := NewStats(3)
	bytes := []int64{50, 500, 10, 999, 250, 700}
	for i, b := range bytes {
		key := "k" + string(rune('a'+i))
		s.Record(key, "ns", "t", obs(b, -1))
	}

	big := s.Biggest()
	if len(big) != 3 {
		t.Fatalf("biggest len: got %d, want 3", len(big))
	}
	if big[0].Bytes != 999 {
		t.Errorf("big[0]: got %d, want 999", big[0].Bytes)
	}
	if big[1].Bytes != 700 {
		t.Errorf("big[1]: got %d, want 700", big[1].Bytes)
	}
	if big[2].Bytes != 500 {
		t.Errorf("big[2]: got %d, want 500", big[2].Bytes)
	}
}

func TestBiggestDisabledWhenCapZero(t *testing.T) {
	s := NewStats(0)
	s.Record("k", "ns", "t", obs(123, -1))
	if len(s.Biggest()) != 0 {
		t.Errorf("biggest should be empty when cap is 0")
	}
}

func TestNamespaceCountAndBytes(t *testing.T) {
	s := NewStats(0)
	s.Record("ns:foo", "ns", "foo", obs(100, -1))
	s.Record("ns:bar", "ns", "bar", obs(200, 60))

	if s.NamespaceCount("ns") != 2 {
		t.Errorf("namespace_count: got %d", s.NamespaceCount("ns"))
	}
	if s.NamespaceBytes("ns") != 300 {
		t.Errorf("namespace_bytes: got %d", s.NamespaceBytes("ns"))
	}
	if s.NamespaceCount("nonexistent") != 0 {
		t.Error("nonexistent namespace count should be 0")
	}
}

func TestPersistentRows(t *testing.T) {
	s := NewStats(0)
	s.Record("ns:config-1", "ns", "config", obs(1000, -1))
	s.Record("ns:config-2", "ns", "config", obs(500, -1))
	s.Record("ns:page-1", "ns", "page", obs(9999, 60))

	rows := PersistentRows(s)
	if len(rows) != 1 {
		t.Fatalf("persistent rows: got %d, want 1", len(rows))
	}
	if rows[0].KeyType != "config" {
		t.Errorf("key_type: got %q", rows[0].KeyType)
	}
	if rows[0].Keys != 2 {
		t.Errorf("keys: got %d", rows[0].Keys)
	}
	if rows[0].Bytes != 1500 {
		t.Errorf("bytes: got %d", rows[0].Bytes)
	}
}
