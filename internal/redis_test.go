package internal

import (
	"context"
	"os"
	"testing"

	"github.com/redis/go-redis/v9"
)

// Helper to create a test connection.
func testConn(t *testing.T) *redis.Client {
	t.Helper()
	url := "redis://127.0.0.1:6379/15"
	if env := os.Getenv("TEST_REDIS_URL"); env != "" {
		url = env
	}
	client, err := ConnectRedis(url, 2)
	if err != nil {
		t.Skipf("skipping: no Redis at %s: %v", url, err)
	}
	t.Cleanup(func() { client.Close() })
	return client
}

func TestConnectRedis(t *testing.T) {
	client, err := ConnectRedis("redis://127.0.0.1:6379/15", 2)
	if err != nil {
		t.Skipf("skipping: no Redis: %v", err)
	}
	defer client.Close()
	if err := client.Ping(context.Background()).Err(); err != nil {
		t.Fatalf("ping failed: %v", err)
	}
}

func TestConnectRedisInvalidURL(t *testing.T) {
	_, err := ConnectRedis("not-a-valid-url", 2)
	if err == nil {
		t.Fatal("expected error for invalid URL")
	}
}

func TestConnectRedisFailsFast(t *testing.T) {
	_, err := ConnectRedis("redis://127.0.0.1:1", 2)
	if err == nil {
		t.Fatal("expected connection error for refused port")
	}
}
