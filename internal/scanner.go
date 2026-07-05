package internal

import (
	"context"
	"fmt"

	"github.com/redis/go-redis/v9"
)

// ScanOptions controls the scanning behavior.
type ScanOptions struct {
	// Count hint for SCAN — keys per server-side step.
	Count int
	// Treat first N chars as namespace when a key has no ':'.
	NamespaceLen int
	// Stop after this many keys (0 = unlimited).
	Limit int64
	// Collect memory usage per key.
	WantMemory bool
	// Collect Redis data type per key.
	WantTypes bool
	// Collect TTL/expiry stats per key.
	WantTTL bool
	// Retain this many largest keys (0 disables leaderboard).
	Biggest int
}

// OnProgress is called after every SCAN batch with the stats accumulated so far.
type OnProgress func(stats *Stats)

// Scan iterates the keyspace using SCAN (never KEYS), optionally enriching each
// batch with MEMORY USAGE / TYPE / TTL via pipelining.
func Scan(ctx context.Context, client *redis.Client, matchPattern string, opts ScanOptions, onProgress OnProgress) (*Stats, error) {
	stats := NewStats(opts.Biggest)

	var cursor uint64 = 0
	for {
		keys, nextCursor, err := client.Scan(ctx, cursor, matchPattern, int64(opts.Count)).Result()
		if err != nil {
			return nil, fmt.Errorf("SCAN command failed: %w", err)
		}

		if len(keys) > 0 {
			observations, err := enrich(ctx, client, keys, opts)
			if err != nil {
				return nil, err
			}
			for i, key := range keys {
				parsed := ParseKey(key, opts.NamespaceLen)
				stats.Record(key, parsed.Namespace, parsed.KeyType, &observations[i])
			}
			if onProgress != nil {
				onProgress(stats)
			}
		}

		cursor = nextCursor
		if cursor == 0 {
			break
		}
		if opts.Limit != 0 && stats.TotalKeys >= opts.Limit {
			break
		}
	}

	return stats, nil
}

// enrich builds one observation per key. When no enrichment is requested this
// is a cheap no-op. Otherwise all extra commands for the whole batch go out via
// pipelining.
func enrich(ctx context.Context, client *redis.Client, keys []string, opts ScanOptions) ([]KeyObservation, error) {
	observations := make([]KeyObservation, len(keys))

	if !opts.WantMemory && !opts.WantTypes && !opts.WantTTL {
		return observations, nil
	}

	pipe := client.Pipeline()
	memoryCmds := make([]*redis.IntCmd, 0, len(keys))
	typeCmds := make([]*redis.StatusCmd, 0, len(keys))
	ttlCmds := make([]*redis.DurationCmd, 0, len(keys))

	for _, key := range keys {
		if opts.WantMemory {
			memoryCmds = append(memoryCmds, pipe.MemoryUsage(ctx, key))
		}
		if opts.WantTypes {
			typeCmds = append(typeCmds, pipe.Type(ctx, key))
		}
		if opts.WantTTL {
			ttlCmds = append(ttlCmds, pipe.TTL(ctx, key))
		}
	}

	_, _ = pipe.Exec(ctx)

	for i := range observations {
		obs := KeyObservation{}

		if opts.WantMemory {
			idx := 0
			if opts.WantMemory {
				if i < len(memoryCmds) {
					val, err := memoryCmds[i].Result()
					if err == nil {
						obs.Bytes = &val
					}
				}
				idx++
			}
			_ = idx
		}
		if opts.WantTypes {
			if i < len(typeCmds) {
				val, err := typeCmds[i].Result()
				if err == nil {
					obs.DataType = &val
				}
			}
		}
		if opts.WantTTL {
			if i < len(ttlCmds) {
				val, err := ttlCmds[i].Result()
				if err == nil {
					ttl := int64(val.Seconds())
					obs.TTL = &ttl
				}
			}
		}
		observations[i] = obs
	}

	// Re-process memory observables more carefully
	mi := 0
	ti := 0
	tti := 0
	for i := range observations {
		if opts.WantMemory {
			if mi < len(memoryCmds) {
				val, err := memoryCmds[mi].Result()
				if err == nil {
					observations[i].Bytes = &val
				}
				mi++
			}
		}
		if opts.WantTypes {
			if ti < len(typeCmds) {
				val, err := typeCmds[ti].Result()
				if err == nil {
					observations[i].DataType = &val
				}
				ti++
			}
		}
		if opts.WantTTL {
			if tti < len(ttlCmds) {
				val, err := ttlCmds[tti].Result()
				if err == nil {
					ttl := int64(val.Seconds())
					observations[i].TTL = &ttl
				}
				tti++
			}
		}
	}

	return observations, nil
}
