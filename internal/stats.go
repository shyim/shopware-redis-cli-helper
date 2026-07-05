package internal

import (
	"container/heap"
	"sort"
)

// TypeStat holds per-type accumulator.
type TypeStat struct {
	Count           int64            `json:"count"`
	TotalBytes      int64            `json:"total_bytes"`
	Measured        int64            `json:"measured"`
	DataTypes       map[string]int64 `json:"data_types,omitempty"`
	Persistent      int64            `json:"persistent"`
	Expiring        int64            `json:"expiring"`
	PersistentBytes int64            `json:"persistent_bytes"`
}

// AvgBytes returns the average bytes per key.
func (t *TypeStat) AvgBytes() int64 {
	if t.Measured == 0 {
		return 0
	}
	return t.TotalBytes / t.Measured
}

// KeyObservation is one observation gathered for a single key.
type KeyObservation struct {
	Bytes    *int64
	DataType *string
	TTL      *int64 // -1 = no expiry, -2 = missing, >=0 seconds
}

// BigKey is a single key retained in the biggest-keys leaderboard.
type BigKey struct {
	Key      string  `json:"key"`
	Bytes    int64   `json:"bytes"`
	DataType *string `json:"data_type,omitempty"`
	TTL      *int64  `json:"ttl,omitempty"`
}

// heapEntry is used for the min-heap of biggest keys.
type heapEntry struct {
	bytes    int64
	key      string
	dataType *string
	ttl      *int64
}

// bigKeyHeap implements heap.Interface (min-heap).
type bigKeyHeap []heapEntry

func (h bigKeyHeap) Len() int           { return len(h) }
func (h bigKeyHeap) Less(i, j int) bool { return h[i].bytes < h[j].bytes }
func (h bigKeyHeap) Swap(i, j int)      { h[i], h[j] = h[j], h[i] }
func (h *bigKeyHeap) Push(x any)        { *h = append(*h, x.(heapEntry)) }
func (h *bigKeyHeap) Pop() any {
	old := *h
	n := len(old)
	x := old[n-1]
	*h = old[0 : n-1]
	return x
}

// Stats aggregates per-key observations into namespace -> type statistics.
type Stats struct {
	Namespaces  map[string]map[string]*TypeStat `json:"namespaces"`
	TotalKeys   int64                           `json:"total_keys"`
	biggestHeap bigKeyHeap
	biggestCap  int
}

// NewStats creates a Stats that retains the `cap` largest keys (0 disables tracking).
func NewStats(cap int) *Stats {
	return &Stats{
		Namespaces: make(map[string]map[string]*TypeStat),
		biggestCap: cap,
	}
}

// Record adds a key observation to the stats.
func (s *Stats) Record(key, namespace, keyType string, obs *KeyObservation) {
	s.TotalKeys++

	if s.Namespaces[namespace] == nil {
		s.Namespaces[namespace] = make(map[string]*TypeStat)
	}
	ns := s.Namespaces[namespace]

	if ns[keyType] == nil {
		ns[keyType] = &TypeStat{DataTypes: make(map[string]int64)}
	}
	stat := ns[keyType]
	stat.Count++

	if obs.Bytes != nil {
		b := *obs.Bytes
		stat.TotalBytes += b
		stat.Measured++
	}
	if obs.DataType != nil {
		stat.DataTypes[*obs.DataType]++
	}
	if obs.TTL != nil {
		ttl := *obs.TTL
		if ttl == -1 {
			stat.Persistent++
			if obs.Bytes != nil {
				stat.PersistentBytes += *obs.Bytes
			}
		} else if ttl >= 0 {
			stat.Expiring++
		}
	}

	// Offer to the leaderboard
	if obs.Bytes != nil {
		s.recordBigKey(key, *obs.Bytes, obs.DataType, obs.TTL)
	}
}

func (s *Stats) recordBigKey(key string, bytes int64, dataType *string, ttl *int64) {
	if s.biggestCap == 0 {
		return
	}
	// Skip if full and key can't beat the current smallest.
	if s.biggestHeap.Len() >= s.biggestCap {
		if s.biggestHeap[0].bytes >= bytes {
			return
		}
		heap.Pop(&s.biggestHeap)
	}
	dt := dataType
	var t *int64
	if ttl != nil {
		v := *ttl
		t = &v
	}
	heap.Push(&s.biggestHeap, heapEntry{
		bytes:    bytes,
		key:      key,
		dataType: dt,
		ttl:      t,
	})
}

// Biggest returns the retained biggest keys, sorted largest-first.
func (s *Stats) Biggest() []BigKey {
	result := make([]BigKey, 0, s.biggestHeap.Len())
	for _, e := range s.biggestHeap {
		result = append(result, BigKey{
			Key:      e.key,
			Bytes:    e.bytes,
			DataType: e.dataType,
			TTL:      e.ttl,
		})
	}
	sort.Slice(result, func(i, j int) bool {
		if result[i].Bytes != result[j].Bytes {
			return result[i].Bytes > result[j].Bytes
		}
		return result[i].Key < result[j].Key
	})
	return result
}

// NamespaceCount returns the total key count for a namespace.
func (s *Stats) NamespaceCount(ns string) int64 {
	m, ok := s.Namespaces[ns]
	if !ok {
		return 0
	}
	var total int64
	for _, stat := range m {
		total += stat.Count
	}
	return total
}

// NamespaceBytes returns the total memory for a namespace.
func (s *Stats) NamespaceBytes(ns string) int64 {
	m, ok := s.Namespaces[ns]
	if !ok {
		return 0
	}
	var total int64
	for _, stat := range m {
		total += stat.TotalBytes
	}
	return total
}
