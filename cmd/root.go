package cmd

import (
	"fmt"
	"os"

	"github.com/redis/go-redis/v9"
	"github.com/spf13/cobra"

	"github.com/shopware-redis-cli-helper/internal"
)

var (
	redisURL       string
	connectTimeout int
)

// RootCmd is the root cobra command.
var RootCmd = &cobra.Command{
	Use:   "shopware-redis-cli-helper",
	Short: "Inspect and clean up a Shopware/Symfony Redis cache",
	Long: `Inspect and clean up a Shopware/Symfony Redis cache.

Provides four subcommands:
  insights  Scan the DB and explore namespace/key statistics (TUI or report).
  get       Fetch one key and print its decoded value.
  cleanup   Remove orphaned Symfony cache tags (dry-run by default).`,
	SilenceUsage: true,
}

func init() {
	RootCmd.PersistentFlags().StringVar(&redisURL, "url", "redis://127.0.0.1:6379", "Redis connection URL")
	RootCmd.PersistentFlags().IntVar(&connectTimeout, "connect-timeout", 5, "Connection timeout in seconds")
	RootCmd.AddCommand(insightsCmd)
	RootCmd.AddCommand(getCmd)
	RootCmd.AddCommand(cleanupCmd)
}

// GetRedisURL returns the global Redis URL.
func GetRedisURL() string {
	return redisURL
}

// GetConnectTimeout returns the global connect timeout.
func GetConnectTimeout() int {
	return connectTimeout
}

// Execute runs the root command.
func Execute() {
	if err := RootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

// MustConnect establishes a Redis connection or exits.
func MustConnect() *redis.Client {
	client, err := internal.ConnectRedis(redisURL, connectTimeout)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
	return client
}
