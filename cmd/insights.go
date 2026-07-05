package cmd

import (
	"context"
	"fmt"
	"os"

	tea "charm.land/bubbletea/v2"
	"github.com/spf13/cobra"

	"github.com/shopware-redis-cli-helper/internal"
	"github.com/shopware-redis-cli-helper/internal/tui"
)

var (
	insightsPattern      string
	insightsCount        int
	insightsLimit        int64
	insightsNamespaceLen int
	insightsMemory       bool
	insightsTypes        bool
	insightsTTL          bool
	insightsFull         bool
	insightsBiggest      int
	insightsMode         string
)

var insightsCmd = &cobra.Command{
	Use:   "insights",
	Short: "Scan the DB and explore namespace/key statistics",
	Long:  "Scan the DB and explore namespace/key statistics in an interactive TUI or as a text/JSON/Markdown report.",
	RunE:  runInsights,
}

var insightsReportCmd = &cobra.Command{
	Use:   "report",
	Short: "Render a one-shot report to stdout or a file",
	RunE:  runReport,
}

var insightsTUICmd = &cobra.Command{
	Use:   "tui",
	Short: "Interactive live viewer",
	RunE:  runTUI,
}

func init() {
	// Scan flags shared between report and tui.
	for _, cmd := range []*cobra.Command{insightsCmd, insightsReportCmd, insightsTUICmd} {
		cmd.Flags().StringVar(&insightsPattern, "pattern", "", "Only scan keys matching this glob pattern")
		cmd.Flags().IntVar(&insightsCount, "count", 1000, "COUNT hint for SCAN")
		cmd.Flags().Int64Var(&insightsLimit, "limit", 0, "Stop after this many keys")
		cmd.Flags().IntVar(&insightsNamespaceLen, "namespace-len", 10, "Chars for namespace when key has no ':'")
		cmd.Flags().BoolVar(&insightsMemory, "memory", false, "Collect memory usage per key")
		cmd.Flags().BoolVar(&insightsTypes, "types", false, "Collect Redis data type per key")
		cmd.Flags().BoolVar(&insightsTTL, "ttl", false, "Collect TTL/expiry stats per key")
		cmd.Flags().BoolVar(&insightsFull, "full", false, "Shortcut: enable --memory --types --ttl")
		cmd.Flags().IntVar(&insightsBiggest, "biggest", 0, "Track the N biggest keys")
	}

	insightsTUICmd.Flags().StringVar(&insightsMode, "mode", "", "Scan depth: basic or advanced")
	insightsCmd.AddCommand(insightsReportCmd)
	insightsCmd.AddCommand(insightsTUICmd)

	insightsReportCmd.Flags().String("format", "table", "Output format: table, markdown, json")
	insightsReportCmd.Flags().StringP("output", "o", "", "Write to file instead of stdout")
	insightsReportCmd.Flags().Int("top", 25, "Only show top N types per namespace")
}

func runInsights(cmd *cobra.Command, args []string) error {
	// Bare "insights" starts the TUI on a terminal.
	if isTerminal() {
		return runTUI(cmd, args)
	}
	fmt.Fprintln(os.Stderr, "No subcommand given. Run `insights report` for output, or `insights tui` for the interactive viewer.")
	return nil
}

func runTUI(cmd *cobra.Command, args []string) error {
	client, err := internal.ConnectRedis(redisURL, connectTimeout)
	if err != nil {
		return fmt.Errorf("failed to connect to Redis: %w", err)
	}
	defer func() { _ = client.Close() }()

	opts := resolveScanOpts()

	var preset *bool
	if hasExplicitStats() {
		f := false
		preset = &f
	} else if insightsMode == "advanced" {
		t := true
		preset = &t
	} else if insightsMode == "basic" {
		f := false
		preset = &f
	}

	m := tui.NewModel(redisURL, connectTimeout, insightsPattern, opts, preset)
	m.SetClient(client)

	p := tea.NewProgram(m)
	if _, err := p.Run(); err != nil {
		return fmt.Errorf("TUI error: %w", err)
	}

	return nil
}

func runReport(cmd *cobra.Command, args []string) error {
	client := MustConnect()
	defer func() { _ = client.Close() }()

	format, _ := cmd.Flags().GetString("format")
	output, _ := cmd.Flags().GetString("output")
	top, _ := cmd.Flags().GetInt("top")

	opts := resolveScanOpts()

	ctx := context.Background()
	stats, err := internal.Scan(ctx, client, insightsPattern, opts, nil)
	if err != nil {
		return fmt.Errorf("scan failed: %w", err)
	}

	reportOpts := &internal.ReportOptions{
		ShowMemory: opts.WantMemory,
		ShowTypes:  opts.WantTypes,
		ShowTTL:    opts.WantTTL,
		Top:        top,
		Biggest:    opts.Biggest,
	}

	var result string
	switch format {
	case "json":
		result = internal.RenderJSON(stats)
	case "markdown":
		result = internal.RenderMarkdown(stats, reportOpts)
	default: // table
		result = internal.RenderTable(stats, reportOpts)
	}

	if output != "" {
		if err := os.WriteFile(output, []byte(result), 0644); err != nil {
			return fmt.Errorf("failed to write to %s: %w", output, err)
		}
		fmt.Fprintf(os.Stderr, "Wrote report to %s\n", output)
	} else {
		fmt.Print(result)
	}

	return nil
}

func resolveScanOpts() internal.ScanOptions {
	memory := insightsMemory || insightsFull || insightsBiggest > 0
	types := insightsTypes || insightsFull
	ttl := insightsTTL || insightsFull

	return internal.ScanOptions{
		Count:        insightsCount,
		NamespaceLen: insightsNamespaceLen,
		Limit:        insightsLimit,
		WantMemory:   memory,
		WantTypes:    types,
		WantTTL:      ttl,
		Biggest:      insightsBiggest,
	}
}

func hasExplicitStats() bool {
	return insightsMemory || insightsTypes || insightsTTL || insightsFull || insightsBiggest > 0
}

func isTerminal() bool {
	fi, err := os.Stdout.Stat()
	if err != nil {
		return false
	}
	return (fi.Mode() & os.ModeCharDevice) != 0
}
