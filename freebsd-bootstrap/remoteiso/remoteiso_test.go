package remoteiso

import (
	"crypto/x509"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"
)

type roundTripperFunc func(*http.Request) (*http.Response, error)

func (f roundTripperFunc) RoundTrip(req *http.Request) (*http.Response, error) {
	return f(req)
}

type timeoutError struct{}

func (timeoutError) Error() string   { return "network not ready" }
func (timeoutError) Timeout() bool   { return true }
func (timeoutError) Temporary() bool { return true }

func TestHTTPReaderAtRetriesTransientTransportErrors(t *testing.T) {
	attempts := 0
	client := &http.Client{Transport: roundTripperFunc(func(req *http.Request) (*http.Response, error) {
		attempts++
		if attempts < 3 {
			return nil, timeoutError{}
		}
		return &http.Response{
			StatusCode: http.StatusPartialContent,
			Body:       io.NopCloser(strings.NewReader("data")),
			Header:     make(http.Header),
		}, nil
	})}
	reader := &HTTPReaderAt{URL: "https://example.invalid/image.iso", Client: client, RetryDelay: time.Nanosecond}

	buffer := make([]byte, 4)
	n, err := reader.ReadAt(buffer, 0)
	if err != nil {
		t.Fatal(err)
	}
	if n != len(buffer) || string(buffer) != "data" {
		t.Fatalf("ReadAt() = %d, %q; want 4, %q", n, buffer, "data")
	}
	if attempts != 3 {
		t.Fatalf("attempts = %d, want 3", attempts)
	}
}

func TestHTTPReaderAtDoesNotRetryPermanentTransportErrors(t *testing.T) {
	attempts := 0
	client := &http.Client{Transport: roundTripperFunc(func(req *http.Request) (*http.Response, error) {
		attempts++
		return nil, x509.UnknownAuthorityError{}
	})}
	reader := &HTTPReaderAt{URL: "https://example.invalid/image.iso", Client: client}

	_, err := reader.ReadAt(make([]byte, 4), 0)
	if err == nil {
		t.Fatal("ReadAt() succeeded, want certificate error")
	}
	var certificateError x509.UnknownAuthorityError
	if !errors.As(err, &certificateError) {
		t.Fatalf("ReadAt() error = %v, want certificate error", err)
	}
	if attempts != 1 {
		t.Fatalf("attempts = %d, want 1", attempts)
	}
}

func TestWaitForReadyRetriesTransientTransportErrors(t *testing.T) {
	attempts := 0
	client := &http.Client{Transport: roundTripperFunc(func(req *http.Request) (*http.Response, error) {
		attempts++
		if req.Method != http.MethodHead {
			t.Fatalf("request method = %s, want HEAD", req.Method)
		}
		if attempts == 1 {
			return nil, timeoutError{}
		}
		return &http.Response{
			StatusCode: http.StatusOK,
			Body:       http.NoBody,
			Header:     make(http.Header),
		}, nil
	})}

	if err := WaitForReady(client, "https://example.invalid/image.iso"); err != nil {
		t.Fatal(err)
	}
	if attempts != 2 {
		t.Fatalf("attempts = %d, want 2", attempts)
	}
}
