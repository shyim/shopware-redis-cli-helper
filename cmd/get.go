package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/dustin/go-humanize"
	"github.com/spf13/cobra"

	"github.com/shopware-redis-cli-helper/internal"
)

var (
	getHex    bool
	getRaw    bool
	getHeader bool
	getJSON   bool
)

var getCmd = &cobra.Command{
	Use:   "get <key>",
	Short: "Fetch one key and print its decoded value",
	Long: `Fetch one key and print its decoded value (auto-decompresses gzip/zlib/zstd).
The \x01tags\x01… escaped form is accepted for tag sets.`,
	Args: cobra.ExactArgs(1),
	RunE: runGet,
}

func init() {
	getCmd.Flags().BoolVar(&getHex, "hex", false, "Print value as hex dump instead of decoded text")
	getCmd.Flags().BoolVar(&getRaw, "raw", false, "Print only the value body (no header)")
	getCmd.Flags().BoolVar(&getHeader, "header", false, "Always print the metadata header, even when piped")
	getCmd.Flags().BoolVar(&getJSON, "json", false, "Emit a JSON object with the value and its metadata")
	getCmd.MarkFlagsMutuallyExclusive("raw", "header")
}

func runGet(cmd *cobra.Command, args []string) error {
	key := args[0]
	client := MustConnect()
	defer client.Close()

	ctx := context.Background()
	v := internal.GetValue(ctx, client, key, internal.ValueDisplayCap)

	if v.Error != "" {
		return fmt.Errorf("%s", v.Error)
	}

	if getJSON {
		return printGetJSON(v)
	}

	showHeader := getHeader || (!getRaw && isTerminal())
	if showHeader {
		fmt.Fprintf(os.Stderr, "%s\n", headerLine(v))
	}

	if getHex {
		fmt.Print(v.Hex)
	} else {
		fmt.Println(v.Body)
	}

	return nil
}

func headerLine(v *internal.KeyValue) string {
	line := fmt.Sprintf("# type=%s", v.RedisType)
	if v.SizeBytes != nil {
		line += fmt.Sprintf("  size=%s", humanize.Bytes(uint64(*v.SizeBytes)))
	}
	if v.Elements != nil {
		line += fmt.Sprintf("  elements=%d", *v.Elements)
	}
	line += fmt.Sprintf("  ttl=%s", internal.TTLWithExpiry(v.TTL))
	if v.Encoding != internal.EncodingPlain {
		label := fmt.Sprintf("decompressed=%s", v.Encoding.Label())
		if v.StreamOffset != nil && *v.StreamOffset > 0 {
			label = fmt.Sprintf("decompressed=%s (embedded @ %d)", v.Encoding.Label(), *v.StreamOffset)
		}
		line += "  " + label
	}
	if v.IsBinary {
		line += "  binary"
	}
	return line
}

func printGetJSON(v *internal.KeyValue) error {
	type out struct {
		Key             string  `json:"key"`
		Type            string  `json:"type"`
		TTL             *int64  `json:"ttl"`
		ExpiresAt       *string `json:"expires_at,omitempty"`
		SizeBytes       *int64  `json:"size_bytes,omitempty"`
		Elements        *int64  `json:"elements,omitempty"`
		Encoding        string  `json:"encoding,omitempty"`
		EmbeddedOffset  *int    `json:"embedded_offset,omitempty"`
		IsBinary        bool    `json:"is_binary"`
		Value           string  `json:"value"`
	}

	o := out{
		Key:      v.Key,
		Type:     v.RedisType,
		TTL:      v.TTL,
		SizeBytes: v.SizeBytes,
		Elements: v.Elements,
		Encoding: v.Encoding.Label(),
		IsBinary: v.IsBinary,
		Value:    v.Body,
	}

	if v.StreamOffset != nil && *v.StreamOffset > 0 {
		o.EmbeddedOffset = v.StreamOffset
	}

	if v.TTL != nil && *v.TTL >= 0 {
		exp := time.Now().Add(time.Duration(*v.TTL) * time.Second).Format(time.RFC3339)
		o.ExpiresAt = &exp
	}

	b, _ := json.MarshalIndent(o, "", "  ")
	fmt.Println(string(b))
	return nil
}
