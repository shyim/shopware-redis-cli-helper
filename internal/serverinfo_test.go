package internal

import "testing"

const sampleInfo = `# Server
redis_version:7.2.4
uptime_in_seconds:263467
# Clients
connected_clients:4
# Memory
used_memory:6073440
used_memory_human:5.79M
maxmemory:104857600
maxmemory_policy:allkeys-lru
mem_fragmentation_ratio:3.22
# Stats
evicted_keys:128
`

func TestParseInfoRelevantFields(t *testing.T) {
	info := ParseInfo(sampleInfo)
	if info.RedisVersion != "7.2.4" {
		t.Errorf("redis_version: got %q", info.RedisVersion)
	}
	if info.UsedMemory != 6073440 {
		t.Errorf("used_memory: got %d", info.UsedMemory)
	}
	if info.Maxmemory != 104857600 {
		t.Errorf("maxmemory: got %d", info.Maxmemory)
	}
	if info.MaxmemoryPolicy != "allkeys-lru" {
		t.Errorf("maxmemory_policy: got %q", info.MaxmemoryPolicy)
	}
	if info.EvictedKeys != 128 {
		t.Errorf("evicted_keys: got %d", info.EvictedKeys)
	}
	if info.ConnectedClients != 4 {
		t.Errorf("connected_clients: got %d", info.ConnectedClients)
	}
	if info.MemFragmentationRatio != 3.22 {
		t.Errorf("mem_fragmentation_ratio: got %f", info.MemFragmentationRatio)
	}
}

func TestMemoryFullnessComputedWhenCapped(t *testing.T) {
	info := ParseInfo(sampleInfo)
	pct := info.MemoryFullness()
	if pct == nil {
		t.Fatal("memory_fullness should not be nil")
	}
	expected := 6073440.0 / 104857600.0
	if *pct < expected-0.001 || *pct > expected+0.001 {
		t.Errorf("memory_fullness: got %f, want ~%f", *pct, expected)
	}
}

func TestMemoryFullnessNoneWhenUnlimited(t *testing.T) {
	info := ParseInfo("used_memory:1000\nmaxmemory:0\n")
	if pct := info.MemoryFullness(); pct != nil {
		t.Errorf("memory_fullness should be nil when maxmemory is 0: got %f", *pct)
	}
}

func TestIgnoresSectionHeadersAndBlanks(t *testing.T) {
	info := ParseInfo("# Server\n\nredis_version:1.2.3\n\n")
	if info.RedisVersion != "1.2.3" {
		t.Errorf("redis_version: got %q", info.RedisVersion)
	}
}
