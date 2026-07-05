package internal

import (
	"context"
	"fmt"
	"time"

	"github.com/redis/go-redis/v9"
)

// ConnectRedis establishes a Redis connection with a bounded timeout so a wrong
// host/port fails fast instead of hanging on the OS TCP timeout.
func ConnectRedis(url string, timeoutSecs int) (*redis.Client, error) {
	if timeoutSecs < 1 {
		timeoutSecs = 1
	}
	opts, err := redis.ParseURL(url)
	if err != nil {
		return nil, fmt.Errorf("invalid redis url %s: %w", url, err)
	}
	opts.DialTimeout = time.Duration(timeoutSecs) * time.Second
	opts.ReadTimeout = time.Duration(timeoutSecs) * time.Second
	opts.WriteTimeout = time.Duration(timeoutSecs) * time.Second
	opts.MaxRetries = 1

	client := redis.NewClient(opts)

	// Verify connectivity with a bounded timeout.
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(timeoutSecs*2)*time.Second)
	defer cancel()

	if err := client.Ping(ctx).Err(); err != nil {
		_ = client.Close()
		return nil, fmt.Errorf("could not connect to redis: %w", err)
	}

	return client, nil
}
