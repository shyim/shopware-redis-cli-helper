package internal

import (
	"context"
	"testing"
)



func TestConnectRedis(t *testing.T) {
	client, err := ConnectRedis("redis://127.0.0.1:6379/15", 2)
	if err != nil {
		t.Skipf("skipping: no Redis: %v", err)
	}
	defer func() { _ = client.Close() }()
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
