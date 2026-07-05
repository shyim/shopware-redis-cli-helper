package internal

import (
	"context"
	"fmt"
	"time"

	"github.com/redis/go-redis/v9"
)

const (
	// KeySampleCap is how many keys a drill-down lists at most.
	KeySampleCap = 1000
	// ValueDisplayCap is how many characters/bytes of a value we render.
	ValueDisplayCap = 20000
	// CollectionCap is how many elements of a collection we show.
	CollectionCap = 500
)

// KeyList is the result of a key listing.
type KeyList struct {
	Namespace string   `json:"namespace"`
	KeyType   string   `json:"key_type"`
	Keys      []string `json:"keys"`
	Capped    bool     `json:"capped"`
	Error     string   `json:"error,omitempty"`
}

// KeyValue is the result of a value fetch.
type KeyValue struct {
	Key           string     `json:"key"`
	RedisType     string     `json:"type"`
	TTL           *int64     `json:"ttl"`
	SizeBytes     *int64     `json:"size_bytes,omitempty"`
	Elements      *int64     `json:"elements,omitempty"`
	Body          string     `json:"body"`
	Hex           string     `json:"hex,omitempty"`
	IsBinary      bool       `json:"is_binary"`
	Encoding      Encoding   `json:"encoding,omitempty"`
	StreamOffset  *int       `json:"stream_offset,omitempty"`
	Error         string     `json:"error,omitempty"`
}

// TTLWithExpiry formats a TTL for display: remaining time plus absolute expiry.
func TTLWithExpiry(ttl *int64) string {
	if ttl == nil {
		return "-"
	}
	switch *ttl {
	case -1:
		return "persistent"
	case -2:
		return "-"
	}
	secs := *ttl
	if secs < 0 {
		return "-"
	}
	expires := time.Now().Add(time.Duration(secs) * time.Second)
	return fmt.Sprintf("%ds (expires %s)", secs, expires.Format("2006-01-02 15:04:05"))
}

// ListKeys returns sample keys belonging to a (namespace, type).
func ListKeys(ctx context.Context, client *redis.Client, namespace, keyType string) *KeyList {
	pattern := MatchPatternFor(namespace, keyType)
	result := &KeyList{
		Namespace: namespace,
		KeyType:   keyType,
	}

	var keys []string
	var cursor uint64
	capped := false

	for {
		res, nextCursor, err := client.Scan(ctx, cursor, pattern, 500).Result()
		if err != nil {
			result.Error = err.Error()
			result.Keys = keys
			return result
		}

		for _, k := range res {
			keys = append(keys, EscapeKey([]byte(k)))
			if len(keys) >= KeySampleCap {
				capped = true
				break
			}
		}

		cursor = nextCursor
		if cursor == 0 || capped {
			break
		}
	}

	// Sort keys
	sortStrings(keys)

	result.Keys = keys
	result.Capped = capped
	return result
}

// GetValue fetches and decodes a single key's value.
func GetValue(ctx context.Context, client *redis.Client, key string, cap int) *KeyValue {
	rawKey := UnescapeKey(key)

	redisType, err := client.Type(ctx, string(rawKey)).Result()
	if err != nil {
		redisType = "unknown"
	}

	ttlDuration, _ := client.TTL(ctx, string(rawKey)).Result()
	var ttl *int64
	if ttlDuration >= 0 || ttlDuration == -1*time.Second {
		v := int64(ttlDuration.Seconds())
		ttl = &v
	} else if ttlDuration == -2*time.Second {
		v := int64(-2)
		ttl = &v
	}

	var sizeBytes *int64
	if mem, err := client.MemoryUsage(ctx, string(rawKey)).Result(); err == nil {
		sizeBytes = &mem
	}

	out := &KeyValue{
		Key:       key,
		RedisType: redisType,
		TTL:       ttl,
		SizeBytes: sizeBytes,
	}

	switch redisType {
	case "string":
		raw, err := client.Get(ctx, string(rawKey)).Bytes()
		if err != nil {
			out.Error = err.Error()
			return out
		}
		decoded := DecodeValue(raw, cap)
		out.Encoding = decoded.Encoding
		out.StreamOffset = decoded.StreamOffset
		out.IsBinary = decoded.IsBinary
		out.Body = decoded.Text
		out.Hex = decoded.Hex

	case "set":
		readCollection(ctx, client, rawKey, "SMEMBERS", false, cap, out)

	case "list":
		readCollection(ctx, client, rawKey, "LRANGE", true, cap, out)

	case "hash":
		readHash(ctx, client, rawKey, cap, out)

	case "zset":
		readZSet(ctx, client, rawKey, cap, out)

	case "none":
		out.Error = "key does not exist (it may have expired)"

	default:
		out.Error = fmt.Sprintf("unsupported type: %s", redisType)
	}

	return out
}

func collCap(cap int) int {
	c := cap / 32
	if c < CollectionCap {
		return CollectionCap
	}
	return c
}

func readCollection(ctx context.Context, client *redis.Client, rawKey []byte, cmd string, isList bool, cap int, out *KeyValue) {
	limit := collCap(cap)
	var members []string
	var err error

	if isList {
		members, err = client.LRange(ctx, string(rawKey), 0, int64(limit-1)).Result()
	} else {
		members, err = client.SMembers(ctx, string(rawKey)).Result()
	}

	if err != nil {
		out.Error = err.Error()
		return
	}

	n := int64(len(members))
	out.Elements = &n

	var body string
	show := members
	if len(show) > limit {
		show = show[:limit]
	}
	for i, m := range show {
		escaped := EscapeKey([]byte(m))
		if i > 0 {
			body += "\n"
		}
		body += escaped
	}
	if len(members) > limit {
		body += fmt.Sprintf("\n… %d more (truncated)", len(members)-limit)
	}
	out.Body = body
}

func readHash(ctx context.Context, client *redis.Client, rawKey []byte, cap int, out *KeyValue) {
	limit := collCap(cap)
	pairs, err := client.HGetAll(ctx, string(rawKey)).Result()
	if err != nil {
		out.Error = err.Error()
		return
	}

	n := int64(len(pairs))
	out.Elements = &n

	var body string
	count := 0
	for field, val := range pairs {
		if count >= limit {
			break
		}
		if count > 0 {
			body += "\n"
		}
		body += fmt.Sprintf("%s = %s", field, val)
		count++
	}
	if len(pairs) > limit {
		body += fmt.Sprintf("\n… %d more fields (truncated)", len(pairs)-limit)
	}
	out.Body = body
}

func readZSet(ctx context.Context, client *redis.Client, rawKey []byte, cap int, out *KeyValue) {
	limit := collCap(cap)
	members, err := client.ZRangeWithScores(ctx, string(rawKey), 0, int64(limit-1)).Result()
	if err != nil {
		out.Error = err.Error()
		return
	}

	n := int64(len(members))
	out.Elements = &n

	var body string
	for i, m := range members {
		if i > 0 {
			body += "\n"
		}
		body += fmt.Sprintf("%g  %s", m.Score, EscapeKey([]byte(m.Member.(string))))
	}
	out.Body = body
}

func sortStrings(s []string) {
	// Simple bubble sort for small lists; the sample cap is at most 1000.
	n := len(s)
	for i := 0; i < n; i++ {
		for j := 0; j < n-i-1; j++ {
			if s[j] > s[j+1] {
				s[j], s[j+1] = s[j+1], s[j]
			}
		}
	}
}
