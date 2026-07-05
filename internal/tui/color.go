package tui

import (
	"fmt"
	"strings"

	"charm.land/lipgloss/v2"
	"charm.land/lipgloss/v2/compat"
)

// ==========================================================================
// Design system — adaptive light/dark palette
// ==========================================================================

var (
	colPrimary   = compat.AdaptiveColor{Light: lipgloss.Color("#189EFF"), Dark: lipgloss.Color("#189EFF")} // Shopware Blue
	colSuccess   = compat.AdaptiveColor{Light: lipgloss.Color("#10B981"), Dark: lipgloss.Color("#10B981")} // Emerald
	colWarning   = compat.AdaptiveColor{Light: lipgloss.Color("#F59E0B"), Dark: lipgloss.Color("#F59E0B")} // Amber
	colDanger    = compat.AdaptiveColor{Light: lipgloss.Color("#EF4444"), Dark: lipgloss.Color("#EF4444")} // Red
	colAccent    = compat.AdaptiveColor{Light: lipgloss.Color("#8B5CF6"), Dark: lipgloss.Color("#A78BFA")} // Violet
	colText      = compat.AdaptiveColor{Light: lipgloss.Color("#1F2937"), Dark: lipgloss.Color("#F3F4F6")}
	colTextDim   = compat.AdaptiveColor{Light: lipgloss.Color("#4B5563"), Dark: lipgloss.Color("#9CA3AF")}
	colTextMuted = compat.AdaptiveColor{Light: lipgloss.Color("#9CA3AF"), Dark: lipgloss.Color("#4B5563")}
	colBorder    = compat.AdaptiveColor{Light: lipgloss.Color("#D1D5DB"), Dark: lipgloss.Color("#374151")}
	colSelBg     = compat.AdaptiveColor{Light: lipgloss.Color("#189EFF"), Dark: lipgloss.Color("#1D4ED8")} // Shopware/Blue highlight
	colSelFg     = compat.AdaptiveColor{Light: lipgloss.Color("#FFFFFF"), Dark: lipgloss.Color("#FFFFFF")}
)

// Style presets
var (
	textBold   = lipgloss.NewStyle().Bold(true)
	textDim    = lipgloss.NewStyle().Foreground(colTextDim)
	textMuted  = lipgloss.NewStyle().Foreground(colTextMuted)
	textNormal = lipgloss.NewStyle().Foreground(colText)

	primary     = lipgloss.NewStyle().Foreground(colPrimary)
	success     = lipgloss.NewStyle().Foreground(colSuccess)
	warning     = lipgloss.NewStyle().Foreground(colWarning)
	danger      = lipgloss.NewStyle().Foreground(colDanger)
	accent      = lipgloss.NewStyle().Foreground(colAccent)
	primaryBold = lipgloss.NewStyle().Foreground(colPrimary).Bold(true)
	successBold = lipgloss.NewStyle().Foreground(colSuccess).Bold(true)
	warningBold = lipgloss.NewStyle().Foreground(colWarning).Bold(true)
	dangerBold  = lipgloss.NewStyle().Foreground(colDanger).Bold(true)

	// Selection — bold blue on a dark blue background bar
	selected = lipgloss.NewStyle().Foreground(colSelFg).Background(colSelBg).Bold(true)

	// Tag badge
	tagBadge = lipgloss.NewStyle().
			Foreground(colWarning).
			Bold(true)
	tagBadgeSel = lipgloss.NewStyle().
			Foreground(compat.AdaptiveColor{Light: lipgloss.Color("#FFFFFF"), Dark: lipgloss.Color("#000000")}).
			Background(colWarning).
			Bold(true)
)

// ==========================================================================
// Layout helpers
// ==========================================================================

func condColor(cond bool, trueColor, falseColor compat.AdaptiveColor) compat.AdaptiveColor {
	if cond {
		return trueColor
	}
	return falseColor
}

// box renders a bordered panel with the title embedded in the top border.
// w/h are the OUTER dimensions (including border).
func box(title string, active bool, w, h int, content string) string {
	if w < 4 {
		w = 4
	}
	if h < 3 {
		h = 3
	}

	bc := colBorder
	if active {
		bc = colPrimary
	}

	borderStyle := lipgloss.NewStyle().Foreground(bc)

	// Inner content area: w minus 2 border chars, minus 2 padding (1 each side)
	innerW := w - 4
	if innerW < 1 {
		innerW = 1
	}

	// Available content lines: h minus 2 border lines
	availLines := h - 2
	if availLines < 0 {
		availLines = 0
	}

	// Split content into lines and pad each to innerW
	contentLines := strings.Split(content, "\n")
	var body []string
	for i := 0; i < availLines; i++ {
		if i < len(contentLines) {
			body = append(body, padRight(contentLines[i], innerW))
		} else {
			body = append(body, strings.Repeat(" ", innerW))
		}
	}

	// Build border lines manually for precise control
	var cornerTL, cornerTR, cornerBL, cornerBR, sideV, sideH string
	if active {
		cornerTL = borderStyle.Render("╔")
		cornerTR = borderStyle.Render("╗")
		cornerBL = borderStyle.Render("╚")
		cornerBR = borderStyle.Render("╝")
		sideV = borderStyle.Render("║")
		sideH = "═"
	} else {
		cornerTL = borderStyle.Render("╭")
		cornerTR = borderStyle.Render("╮")
		cornerBL = borderStyle.Render("╰")
		cornerBR = borderStyle.Render("╯")
		sideV = borderStyle.Render("│")
		sideH = "─"
	}

	// Top border with title
	titleStr := ""
	titleWidth := 0
	if title != "" {
		if active {
			titleStr = lipgloss.NewStyle().Foreground(colSelFg).Background(colPrimary).Bold(true).Render(" " + title + " ")
		} else {
			titleStr = lipgloss.NewStyle().Foreground(colTextDim).Bold(true).Render(" " + title + " ")
		}
		titleWidth = lipgloss.Width(" " + title + " ")
	}

	topDashesCount := w - 2 - titleWidth - 1 // 1 for the initial dash
	if topDashesCount < 0 {
		topDashesCount = 0
	}
	initialDash := borderStyle.Render(sideH)
	restDashes := borderStyle.Render(strings.Repeat(sideH, topDashesCount))

	topBorder := cornerTL + initialDash + titleStr + restDashes + cornerTR
	// Pad top border to full width if title was wider than available
	topW := lipgloss.Width(topBorder)
	if topW < w {
		topBorder += strings.Repeat(" ", w-topW)
	}

	// Bottom border
	bottomDashes := borderStyle.Render(strings.Repeat(sideH, w-2))
	bottomBorder := cornerBL + bottomDashes + cornerBR

	// Middle lines with side borders
	var middle []string
	for _, line := range body {
		middle = append(middle, sideV+" "+line+" "+sideV)
	}

	allLines := append([]string{topBorder}, middle...)
	allLines = append(allLines, bottomBorder)

	return strings.Join(allLines, "\n")
}

// ==========================================================================
// String helpers
// ==========================================================================

func padRight(s string, n int) string {
	w := lipgloss.Width(s)
	if w >= n {
		return s
	}
	return s + strings.Repeat(" ", n-w)
}


func trunc(s string, max int) string {
	if lipgloss.Width(s) <= max {
		return s
	}
	r := []rune(s)
	if max <= 1 {
		return "…"
	}
	// Truncate by display width
	result := ""
	width := 0
	for _, ch := range r {
		chW := lipgloss.Width(string(ch))
		if width+chW >= max {
			result += "…"
			break
		}
		result += string(ch)
		width += chW
	}
	return result
}


// ==========================================================================
// Shared formatting
// ==========================================================================

func pctFmt(part, whole int64) string {
	if whole == 0 {
		return "  0.0%"
	}
	return fmt.Sprintf("%5.1f%%", float64(part)/float64(whole)*100.0)
}

func ttlLabel(ttl *int64) string {
	if ttl == nil {
		return "-"
	}
	switch *ttl {
	case -1:
		return "persistent"
	case -2:
		return "-"
	}
	if *ttl >= 0 {
		return fmt.Sprintf("%ds", *ttl)
	}
	return "-"
}

var spinnerFrames = []string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"}
var spinnerIdx int

func spinnerGlyph() string {
	return spinnerFrames[spinnerIdx%len(spinnerFrames)]
}
