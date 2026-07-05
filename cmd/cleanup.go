package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sync"

	"github.com/redis/go-redis/v9"
	"github.com/spf13/cobra"
)

var (
	cleanupNamespace   string
	cleanupApply       bool
	cleanupConcurrency int
	cleanupCount       int
	cleanupBatchSize   int
	cleanupJSON        bool
)

const tagInfix = "\x01tags\x01"

var cleanupCmd = &cobra.Command{
	Use:   "cleanup",
	Short: "Remove orphaned Symfony cache tags (dry-run by default)",
	Long: `Remove orphaned Symfony cache tags.

Symfony's cache tag sets accumulate members that point to expired keys.
This command scans the tag sets and prunes orphaned members, deleting
empty sets. Runs as a dry run by default — re-run with --apply to clean.`,
	RunE: runCleanup,
}

func init() {
	cleanupCmd.Flags().StringVar(&cleanupNamespace, "namespace", "", "Scope to a single namespace prefix")
	cleanupCmd.Flags().BoolVar(&cleanupApply, "apply", false, "Actually delete orphaned members and empty sets")
	cleanupCmd.Flags().IntVar(&cleanupConcurrency, "concurrency", 8, "Number of concurrent workers")
	cleanupCmd.Flags().IntVar(&cleanupCount, "count", 1000, "COUNT hint for SCAN")
	cleanupCmd.Flags().IntVar(&cleanupBatchSize, "batch-size", 100, "Tag keys per Lua invocation")
	cleanupCmd.Flags().BoolVar(&cleanupJSON, "json", false, "Emit JSON summary")
}

func matchPattern(namespace string) string {
	if namespace != "" {
		return fmt.Sprintf("%s:%s*", namespace, tagInfix)
	}
	return fmt.Sprintf("*%s*", tagInfix)
}

type totals struct {
	ProcessedKeys  int64 `json:"processed_keys"`
	RemovedMembers int64 `json:"removed_members"`
	DeletedKeys    int64 `json:"deleted_keys"`
}

func (t *totals) add(o totals) {
	t.ProcessedKeys += o.ProcessedKeys
	t.RemovedMembers += o.RemovedMembers
	t.DeletedKeys += o.DeletedKeys
}

func runCleanup(cmd *cobra.Command, args []string) error {
	scope := "all namespaces"
	if cleanupNamespace != "" {
		scope = fmt.Sprintf("namespace '%s'", cleanupNamespace)
	}

	dryRun := !cleanupApply
	pattern := matchPattern(cleanupNamespace)

	if !cleanupJSON {
		fmt.Fprintf(os.Stderr, "Redis tag cleanup — %s%s\n", scope, map[bool]string{true: " (DRY RUN)", false: ""}[dryRun])
		if dryRun {
			fmt.Fprintln(os.Stderr, "No keys will be modified. Re-run with --apply to clean.")
		}
	}

	client := MustConnect()
	defer func() { _ = client.Close() }()

	ctx := context.Background()

	// Channel for batches
	batchCh := make(chan []string, cleanupConcurrency*2)

	var wg sync.WaitGroup
	var mu sync.Mutex
	var grandTotals totals
	var errCount int

	// Workers
	for i := 0; i < cleanupConcurrency; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			worker := client // each worker can share the connection; go-redis is safe
			for batch := range batchCh {
				t, err := processBatch(ctx, worker, batch, dryRun)
				if err != nil {
					fmt.Fprintf(os.Stderr, "batch error: %v\n", err)
					errCount++
					continue
				}
				mu.Lock()
				grandTotals.add(t)
				mu.Unlock()
			}
			_ = worker
		}()
	}

	// Producer: SCAN
	var cursor uint64
	var pending []string
	for {
		keys, nextCursor, err := client.Scan(ctx, cursor, pattern, int64(cleanupCount)).Result()
		if err != nil {
			return fmt.Errorf("SCAN failed: %w", err)
		}

		for _, k := range keys {
			pending = append(pending, k)
			if len(pending) >= cleanupBatchSize {
				batch := make([]string, len(pending))
				copy(batch, pending)
				pending = pending[:0]
				batchCh <- batch
			}
		}

		cursor = nextCursor
		if cursor == 0 {
			break
		}
	}
	if len(pending) > 0 {
		batchCh <- pending
	}
	close(batchCh)

	wg.Wait()

	if cleanupJSON {
		type out struct {
			DryRun bool   `json:"dry_run"`
			Totals totals `json:"totals"`
		}
		o := out{DryRun: dryRun, Totals: grandTotals}
		b, _ := json.MarshalIndent(o, "", "  ")
		fmt.Println(string(b))
		return nil
	}

	fmt.Println()
	fmt.Printf("Tag keys processed:  %d\n", grandTotals.ProcessedKeys)
	if dryRun {
		fmt.Printf("Orphaned members:    %d (would remove)\n", grandTotals.RemovedMembers)
		fmt.Printf("Empty tag sets:      %d (would delete)\n", grandTotals.DeletedKeys)
		fmt.Println("\nDry run complete — re-run with --apply to perform the cleanup.")
	} else {
		fmt.Printf("Orphaned members removed: %d\n", grandTotals.RemovedMembers)
		fmt.Printf("Empty tag sets deleted:   %d\n", grandTotals.DeletedKeys)
		fmt.Println("\nCleanup complete.")
	}

	if errCount > 0 {
		fmt.Fprintf(os.Stderr, "\nWarning: %d batch errors occurred.\n", errCount)
	}

	return nil
}

const luaCleanup = `
local dry_run = ARGV[1] == '1'
local CHUNK = 4000

local deleted_keys = 0
local removed_members = 0
local processed_keys = 0

for _, tag_key in ipairs(KEYS) do
    local key_type = redis.call('TYPE', tag_key)
    if key_type and key_type.ok == 'set' then
        processed_keys = processed_keys + 1
        local members = redis.call('SMEMBERS', tag_key)
        local total = #members

        if total == 0 then
            if not dry_run then
                redis.call('DEL', tag_key)
            end
            deleted_keys = deleted_keys + 1
        else
            local alive = 0
            for i = 1, total, CHUNK do
                local args = {}
                for j = i, math.min(i + CHUNK - 1, total) do
                    args[#args + 1] = members[j]
                end
                alive = alive + redis.call('EXISTS', unpack(args))
            end

            local missing = total - alive
            if missing > 0 then
                local to_remove = {}
                for _, member in ipairs(members) do
                    if redis.call('EXISTS', member) == 0 then
                        to_remove[#to_remove + 1] = member
                    end
                end

                if not dry_run then
                    for i = 1, #to_remove, CHUNK do
                        local args = {}
                        for j = i, math.min(i + CHUNK - 1, #to_remove) do
                            args[#args + 1] = to_remove[j]
                        end
                        redis.call('SREM', tag_key, unpack(args))
                    end
                end
                removed_members = removed_members + #to_remove

                if missing == total then
                    if not dry_run then
                        redis.call('DEL', tag_key)
                    end
                    deleted_keys = deleted_keys + 1
                end
            end
        end
    end
end

return {processed_keys, removed_members, deleted_keys}
`

func processBatch(ctx context.Context, client *redis.Client, keys []string, dryRun bool) (totals, error) {
	var t totals
	if len(keys) == 0 {
		return t, nil
	}

	dryRunArg := "0"
	if dryRun {
		dryRunArg = "1"
	}

	res, err := client.Eval(ctx, luaCleanup, keys, dryRunArg).Result()
	if err != nil {
		return t, fmt.Errorf("lua script failed: %w", err)
	}

	slice, ok := res.([]interface{})
	if !ok || len(slice) < 3 {
		return t, fmt.Errorf("unexpected script result format: %v", res)
	}

	t.ProcessedKeys = toInt64(slice[0])
	t.RemovedMembers = toInt64(slice[1])
	t.DeletedKeys = toInt64(slice[2])

	return t, nil
}

func toInt64(v interface{}) int64 {
	switch val := v.(type) {
	case int64:
		return val
	case int:
		return int64(val)
	case int32:
		return int64(val)
	case float64:
		return int64(val)
	default:
		return 0
	}
}
