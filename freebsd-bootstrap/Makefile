# Makefile for cross-compiling freebsd-bootstrap for FreeBSD

# Project configuration
PROJECT_NAME = freebsd-bootstrap
BINARY_NAME = freebsd-bootstrap

# Cross-compilation settings for FreeBSD
GOOS = freebsd
GOARCH = arm64

# Static build flags
CGO_ENABLED = 0
GOTAGS = netgo

# Build flags for fully static binary
LDFLAGS = -ldflags '-extldflags "-static" -w -s'

# Go build command with static flags
BUILD_CMD = CGO_ENABLED=$(CGO_ENABLED) GOOS=$(GOOS) GOARCH=$(GOARCH) go build \
	-tags $(GOTAGS) \
	$(LDFLAGS) \
	-o $(BINARY_NAME) \
	.

.PHONY: all clean freebsd-static build test

# Default target
all: freebsd-static

# Build for FreeBSD with static linking
freebsd-static:
	@echo "Building $(PROJECT_NAME) for FreeBSD (static)..."
	@echo "GOOS=$(GOOS) GOARCH=$(GOARCH) CGO_ENABLED=$(CGO_ENABLED)"
	$(BUILD_CMD)
	@echo "Build complete: $(BINARY_NAME)"
	@file $(BINARY_NAME) || true

# Generic build target (uses freebsd-static)
build: freebsd-static

# Test the project
test:
	@echo "Running tests..."
	go test -v ./...

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	rm -f $(BINARY_NAME)

# Show build info
info:
	@echo "Project: $(PROJECT_NAME)"
	@echo "Target OS: $(GOOS)"
	@echo "Target Architecture: $(GOARCH)"
	@echo "CGO Enabled: $(CGO_ENABLED)"
	@echo "Build Tags: $(GOTAGS)"
	@echo "LDFLAGS: $(LDFLAGS)"

# Verify the binary is statically linked (FreeBSD specific)
verify:
	@if [ -f "$(BINARY_NAME)" ]; then \
		echo "Verifying $(BINARY_NAME)..."; \
		file $(BINARY_NAME); \
		echo ""; \
		echo "Checking for dynamic dependencies:"; \
		objdump -p $(BINARY_NAME) 2>/dev/null | grep NEEDED || echo "No dynamic dependencies found (good for static build)"; \
	else \
		echo "Binary $(BINARY_NAME) not found. Run 'make build' first."; \
	fi

# Help target
help:
	@echo "Available targets:"
	@echo "  all            - Build for FreeBSD (default)"
	@echo "  build          - Build for FreeBSD"
	@echo "  freebsd-static - Build statically linked binary for FreeBSD"
	@echo "  test           - Run tests"
	@echo "  clean          - Remove build artifacts"
	@echo "  info           - Show build configuration"
	@echo "  verify         - Verify the binary is statically linked"
	@echo "  help           - Show this help"
