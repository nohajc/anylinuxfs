package guestnet

import (
	"regexp"
	"strings"
	"testing"
)

func env(values map[string]string) func(string) string {
	return func(name string) string { return values[name] }
}

func validEnv() map[string]string {
	return map[string]string{
		GatewayIPEnv: "10.20.30.1",
		VMIPEnv:      "10.20.30.2",
		PrefixLenEnv: "30",
	}
}

func TestFromEnv(t *testing.T) {
	config, err := FromEnv(env(validEnv()))
	if err != nil {
		t.Fatal(err)
	}
	if got, want := config.InterfaceAddress(), "10.20.30.2/30"; got != want {
		t.Fatalf("InterfaceAddress() = %q, want %q", got, want)
	}
	if got, want := config.ResolvConf(), "nameserver 10.20.30.1\n"; got != want {
		t.Fatalf("ResolvConf() = %q, want %q", got, want)
	}
}

func TestFromEnvRejectsMissingVariable(t *testing.T) {
	values := validEnv()
	delete(values, VMIPEnv)
	if _, err := FromEnv(env(values)); err == nil || !strings.Contains(err.Error(), VMIPEnv) {
		t.Fatalf("FromEnv() error = %v, want error naming %s", err, VMIPEnv)
	}
}

func TestFromEnvRejectsNonIPv4Address(t *testing.T) {
	values := validEnv()
	values[GatewayIPEnv] = "2001:db8::1"
	if _, err := FromEnv(env(values)); err == nil || !strings.Contains(err.Error(), "IPv4") {
		t.Fatalf("FromEnv() error = %v, want IPv4 validation error", err)
	}
}

func TestFromEnvRejectsInvalidPrefix(t *testing.T) {
	for _, prefix := range []string{"invalid", "-1", "33"} {
		values := validEnv()
		values[PrefixLenEnv] = prefix
		if _, err := FromEnv(env(values)); err == nil || !strings.Contains(err.Error(), PrefixLenEnv) {
			t.Errorf("FromEnv() with prefix %q error = %v, want prefix validation error", prefix, err)
		}
	}
}

func TestInitNetworkScriptUsesEnvironment(t *testing.T) {
	for _, name := range []string{GatewayIPEnv, VMIPEnv, PrefixLenEnv} {
		if !strings.Contains(InitNetworkScript, name) {
			t.Errorf("InitNetworkScript does not reference %s", name)
		}
	}
	if regexp.MustCompile(`\b(?:\d{1,3}\.){3}\d{1,3}\b`).MatchString(InitNetworkScript) {
		t.Error("InitNetworkScript contains a hardcoded IPv4 address")
	}
	if !strings.Contains(InitNetworkScript, "ifconfig lo0 up") {
		t.Error("InitNetworkScript does not bring up the loopback interface")
	}
}
