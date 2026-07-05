package internal

import (
	"context"
	"strconv"
	"strings"
	"time"

	"github.com/redis/go-redis/v9"
)

const pollInterval = 2 * time.Second

// ServerInfo is a parsed snapshot of the relevant INFO fields plus DBSIZE.
type ServerInfo struct {
	RedisVersion          string  `json:"redis_version,omitempty"`
	UsedMemory            int64   `json:"used_memory,omitempty"`
	Maxmemory             int64   `json:"maxmemory,omitempty"`
	MaxmemoryPolicy       string  `json:"maxmemory_policy,omitempty"`
	EvictedKeys           int64   `json:"evicted_keys,omitempty"`
	MemFragmentationRatio float64 `json:"mem_fragmentation_ratio,omitempty"`
	ConnectedClients      int64   `json:"connected_clients,omitempty"`
	DBKeys                int64   `json:"db_keys,omitempty"`
}

// MemoryFullness returns the fraction of the memory ceiling in use, if configured.
func (s *ServerInfo) MemoryFullness() *float64 {
	if s.Maxmemory > 0 && s.UsedMemory >= 0 {
		pct := float64(s.UsedMemory) / float64(s.Maxmemory)
		if pct > 1.0 {
			pct = 1.0
		}
		return &pct
	}
	return nil
}

// ParseInfo parses the textual INFO reply into a ServerInfo struct.
func ParseInfo(text string) *ServerInfo {
	info := &ServerInfo{}
	for _, line := range strings.Split(text, "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, ":", 2)
		if len(parts) != 2 {
			continue
		}
		key, val := parts[0], parts[1]
		switch key {
		case "redis_version":
			info.RedisVersion = val
		case "used_memory":
			info.UsedMemory, _ = strconv.ParseInt(val, 10, 64)
		case "maxmemory":
			info.Maxmemory, _ = strconv.ParseInt(val, 10, 64)
		case "maxmemory_policy":
			info.MaxmemoryPolicy = val
		case "evicted_keys":
			info.EvictedKeys, _ = strconv.ParseInt(val, 10, 64)
		case "mem_fragmentation_ratio":
			info.MemFragmentationRatio, _ = strconv.ParseFloat(val, 64)
		case "connected_clients":
			info.ConnectedClients, _ = strconv.ParseInt(val, 10, 64)
		}
	}
	return info
}

// PollServerInfo polls Redis for server info every pollInterval.
// It sends updates on the provided channel.
func PollServerInfo(ctx context.Context, client *redis.Client, ch chan<- *ServerInfo) {
	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			info, err := fetchServerInfo(ctx, client)
			if err == nil {
				select {
				case ch <- info:
				default:
				}
			}
		}
	}
}

func fetchServerInfo(ctx context.Context, client *redis.Client) (*ServerInfo, error) {
	raw, err := client.Info(ctx).Result()
	if err != nil {
		return nil, err
	}
	info := ParseInfo(raw)

	if dbSize, err := client.DBSize(ctx).Result(); err == nil {
		info.DBKeys = dbSize
	}

	return info, nil
}
