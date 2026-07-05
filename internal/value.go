package internal

import (
	"bytes"
	"compress/gzip"
	"compress/zlib"
	"fmt"
	"io"
	"strings"
	"unicode/utf8"

	"github.com/klauspost/compress/zstd"
)

// Encoding represents the compression format detected.
type Encoding int

const (
	EncodingPlain Encoding = iota
	EncodingGzip
	EncodingZlib
	EncodingZstd
)

func (e Encoding) Label() string {
	switch e {
	case EncodingPlain:
		return "plain"
	case EncodingGzip:
		return "gzip"
	case EncodingZlib:
		return "zlib/deflate"
	case EncodingZstd:
		return "zstd"
	}
	return "unknown"
}

// Decoded is a decoded payload ready for display.
type Decoded struct {
	Encoding      Encoding
	StreamOffset  *int // nil for plain, 0 for leading stream, >0 for embedded
	Text          string
	Hex           string
	IsBinary      bool
}

// sniff detects compression from leading magic bytes.
func sniff(raw []byte) Encoding {
	switch {
	case len(raw) >= 2 && raw[0] == 0x1f && raw[1] == 0x8b:
		return EncodingGzip
	case len(raw) >= 4 && raw[0] == 0x28 && raw[1] == 0xb5 && raw[2] == 0x2f && raw[3] == 0xfd:
		return EncodingZstd
	case len(raw) >= 2 && raw[0] == 0x78 && (raw[1] == 0x01 || raw[1] == 0x5e || raw[1] == 0x9c || raw[1] == 0xda):
		return EncodingZlib
	}
	return EncodingPlain
}

func inflateGzip(data []byte) ([]byte, error) {
	r, err := gzip.NewReader(bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	defer r.Close()
	return io.ReadAll(r)
}

func inflateZlib(data []byte) ([]byte, error) {
	r, err := zlib.NewReader(bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	defer r.Close()
	return io.ReadAll(r)
}

func inflateZstd(data []byte) ([]byte, error) {
	// Decode a single frame and stop.
	decoder, err := zstd.NewReader(bytes.NewReader(data), zstd.WithDecoderConcurrency(0))
	if err != nil {
		return nil, err
	}
	defer decoder.Close()
	return io.ReadAll(decoder)
}

func tryDecompress(data []byte, enc Encoding) ([]byte, error) {
	var out []byte
	var err error
	switch enc {
	case EncodingGzip:
		out, err = inflateGzip(data)
	case EncodingZlib:
		out, err = inflateZlib(data)
	case EncodingZstd:
		out, err = inflateZstd(data)
	default:
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if len(out) < 8 {
		return nil, fmt.Errorf("decompressed result too short")
	}
	return out, nil
}

// findEmbedded scans raw for the first embedded compression stream that
// actually inflates. Returns (encoding, offset, inflated).
func findEmbedded(raw []byte) (Encoding, int, []byte) {
	scanEnd := len(raw)
	if scanEnd > 8192 {
		scanEnd = 8192
	}
	for i := 0; i < scanEnd; i++ {
		window := raw[i:]
		enc := sniff(window)
		if enc == EncodingPlain {
			continue
		}
		out, err := tryDecompress(window, enc)
		if err == nil {
			return enc, i, out
		}
	}
	return EncodingPlain, 0, nil
}

// DecodeValue decodes a raw Redis string value: decompress when recognized,
// then render as UTF-8 text or a hex dump. maxText bounds the rendered text
// length so a huge value can't stall the UI.
func DecodeValue(raw []byte, maxText int) Decoded {
	var enc Encoding
	var streamOffset *int
	var bytes []byte

	switch sniff(raw) {
	case EncodingPlain:
		// Not compressed from the front — look for an embedded stream.
		if e, off, out := findEmbedded(raw); out != nil {
			enc = e
			so := off
			streamOffset = &so
			bytes = out
		} else {
			enc = EncodingPlain
			streamOffset = nil
			bytes = raw
		}
	default:
		out, err := tryDecompress(raw, sniff(raw))
		if err == nil {
			enc = sniff(raw)
			so := 0
			streamOffset = &so
			bytes = out
		} else {
			enc = EncodingPlain
			streamOffset = nil
			bytes = raw
		}
	}

	isBinary := !utf8.Valid(bytes)
	text := truncateChars(string(bytes), maxText)
	hexDump := hexDumpFunc(bytes, maxText)

	return Decoded{
		Encoding:     enc,
		StreamOffset: streamOffset,
		Text:         text,
		Hex:          hexDump,
		IsBinary:     isBinary,
	}
}

func truncateChars(s string, max int) string {
	runes := []rune(s)
	if len(runes) <= max {
		return s
	}
	head := string(runes[:max])
	remaining := len(runes) - max
	return fmt.Sprintf("%s\n\n… %d more characters (truncated)", head, remaining)
}

func hexDumpFunc(bytes []byte, max int) string {
	shown := len(bytes)
	if shown > max {
		shown = max
	}
	var out strings.Builder
	for i := 0; i < shown; i += 16 {
		offset := i
		chunk := bytes[i:]
		if len(chunk) > 16 {
			chunk = chunk[:16]
		}
		hexParts := make([]string, len(chunk))
		asciiParts := make([]byte, len(chunk))
		for j, b := range chunk {
			hexParts[j] = fmt.Sprintf("%02x", b)
			if b >= 0x20 && b < 0x7f {
				asciiParts[j] = b
			} else {
				asciiParts[j] = '.'
			}
		}
		hexStr := strings.Join(hexParts, " ")
		out.WriteString(fmt.Sprintf("%08x  %-48s  %s\n", offset, hexStr, string(asciiParts)))
	}
	if len(bytes) > shown {
		out.WriteString(fmt.Sprintf("… %d more bytes (truncated)\n", len(bytes)-shown))
	}
	return out.String()
}
