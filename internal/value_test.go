package internal

import (
	"bytes"
	"compress/gzip"
	"compress/zlib"
	"testing"
)

func gzipData(t *testing.T, data []byte) []byte {
	t.Helper()
	var buf bytes.Buffer
	w := gzip.NewWriter(&buf)
	if _, err := w.Write(data); err != nil {
		t.Fatal(err)
	}
	_ = w.Close()
	return buf.Bytes()
}

func zlibData(t *testing.T, data []byte) []byte {
	t.Helper()
	var buf bytes.Buffer
	w := zlib.NewWriter(&buf)
	if _, err := w.Write(data); err != nil {
		t.Fatal(err)
	}
	_ = w.Close()
	return buf.Bytes()
}

func TestDecodePlainUTF8PassesThrough(t *testing.T) {
	d := DecodeValue([]byte("hello world"), 1000)
	if d.Encoding != EncodingPlain {
		t.Errorf("encoding: got %v, want Plain", d.Encoding)
	}
	if d.Text != "hello world" {
		t.Errorf("text: got %q", d.Text)
	}
	if d.IsBinary {
		t.Error("should not be binary")
	}
}

func TestDecodeDetectsAndInflatesGzip(t *testing.T) {
	raw := gzipData(t, []byte("the quick brown fox"))
	d := DecodeValue(raw, 1000)
	if d.Encoding != EncodingGzip {
		t.Errorf("encoding: got %v, want Gzip", d.Encoding)
	}
	if d.Text != "the quick brown fox" {
		t.Errorf("text: got %q", d.Text)
	}
}

func TestDecodeDetectsAndInflatesZlib(t *testing.T) {
	raw := zlibData(t, []byte("deflate me please"))
	d := DecodeValue(raw, 1000)
	if d.Encoding != EncodingZlib {
		t.Errorf("encoding: got %v, want Zlib", d.Encoding)
	}
	if d.Text != "deflate me please" {
		t.Errorf("text: got %q", d.Text)
	}
}

func TestDecodeFindsZlibStreamEmbeddedAfterWrapper(t *testing.T) {
	inner := []byte("a:2:{s:4:\"core\";a:1:{s:5:\"email\";s:5:\"a@b.c\";}}")
	raw := []byte{0x9d, 0, 0, 0, 0, 0, 0, 0, 0}
	raw = append(raw, []byte("a:1:{i:0;s:13:\"system-config\";}s:2082:\"")...)
	off := len(raw)
	raw = append(raw, zlibData(t, inner)...)

	d := DecodeValue(raw, 10000)
	if d.Encoding != EncodingZlib {
		t.Errorf("encoding: got %v, want Zlib", d.Encoding)
	}
	if d.StreamOffset == nil || *d.StreamOffset != off {
		t.Errorf("stream_offset: got %v, want %d", d.StreamOffset, off)
	}
	if !bytes.Contains([]byte(d.Text), []byte("email")) {
		t.Errorf("inner not decoded: %s", d.Text)
	}
}

func TestDecodeLeadingStreamReportsOffsetZero(t *testing.T) {
	raw := zlibData(t, []byte("plain stream"))
	d := DecodeValue(raw, 1000)
	if d.StreamOffset == nil || *d.StreamOffset != 0 {
		t.Errorf("stream_offset: got %v, want 0", d.StreamOffset)
	}
}

func TestDecodeBinaryWithoutStreamHasNoOffset(t *testing.T) {
	d := DecodeValue([]byte{0x9d, 0x00, 0x01, 0x02, 0xff, 0xfe}, 1000)
	if d.Encoding != EncodingPlain {
		t.Errorf("encoding: got %v, want Plain", d.Encoding)
	}
	if d.StreamOffset != nil {
		t.Errorf("stream_offset should be nil for plain binary")
	}
	if !d.IsBinary {
		t.Error("should be binary")
	}
}

func TestDecodeNonUTF8FlaggedBinary(t *testing.T) {
	d := DecodeValue([]byte{0x00, 0xff, 0xfe, 0x01}, 1000)
	if !d.IsBinary {
		t.Error("should be binary")
	}
	if !bytes.Contains([]byte(d.Hex), []byte("00 ff fe 01")) {
		t.Errorf("hex view missing expected bytes: %s", d.Hex)
	}
}

func TestDecodeUTF8ValueHasHexView(t *testing.T) {
	d := DecodeValue([]byte("hi"), 1000)
	if d.IsBinary {
		t.Error("should not be binary")
	}
	if d.Text != "hi" {
		t.Errorf("text: got %q", d.Text)
	}
	if !bytes.Contains([]byte(d.Hex), []byte("68 69")) {
		t.Errorf("hex view missing: %s", d.Hex)
	}
}

func TestDecodeLongTextTruncated(t *testing.T) {
	s := make([]byte, 5000)
	for i := range s {
		s[i] = 'a'
	}
	d := DecodeValue(s, 100)
	if !bytes.Contains([]byte(d.Text), []byte("truncated")) {
		t.Errorf("should be truncated: %s", d.Text[:200])
	}
}

func TestDecodeCorruptGzipMagicFallsBackToPlain(t *testing.T) {
	d := DecodeValue([]byte{0x1f, 0x8b, 0x00, 0x00}, 1000)
	if d.Encoding != EncodingPlain {
		t.Errorf("encoding: got %v, want Plain", d.Encoding)
	}
}
