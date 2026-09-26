package guestnet

import (
	"fmt"
	"net/netip"
	"os"
	"strconv"
)

const (
	GatewayIPEnv = "ALFS_VM_GATEWAY_IP"
	VMIPEnv      = "ALFS_VM_IP"
	PrefixLenEnv = "ALFS_VM_PREFIX_LEN"
)

type Config struct {
	GatewayIP netip.Addr
	VMIP      netip.Addr
	PrefixLen int
}

func FromEnvironment() (Config, error) {
	return FromEnv(os.Getenv)
}

func FromEnv(getenv func(string) string) (Config, error) {
	gatewayIP, err := parseIPv4(getenv, GatewayIPEnv)
	if err != nil {
		return Config{}, err
	}
	vmIP, err := parseIPv4(getenv, VMIPEnv)
	if err != nil {
		return Config{}, err
	}

	prefixText := getenv(PrefixLenEnv)
	if prefixText == "" {
		return Config{}, fmt.Errorf("required environment variable %s is not set", PrefixLenEnv)
	}
	prefixLen, err := strconv.Atoi(prefixText)
	if err != nil || prefixLen < 0 || prefixLen > 32 {
		return Config{}, fmt.Errorf("environment variable %s must be an IPv4 prefix length from 0 to 32", PrefixLenEnv)
	}

	return Config{GatewayIP: gatewayIP, VMIP: vmIP, PrefixLen: prefixLen}, nil
}

func parseIPv4(getenv func(string) string, name string) (netip.Addr, error) {
	value := getenv(name)
	if value == "" {
		return netip.Addr{}, fmt.Errorf("required environment variable %s is not set", name)
	}
	addr, err := netip.ParseAddr(value)
	if err != nil || !addr.Is4() {
		return netip.Addr{}, fmt.Errorf("environment variable %s must contain a valid IPv4 address", name)
	}
	return addr, nil
}

func (c Config) InterfaceAddress() string {
	return fmt.Sprintf("%s/%d", c.VMIP, c.PrefixLen)
}

func (c Config) ResolvConf() string {
	return fmt.Sprintf("nameserver %s\n", c.GatewayIP)
}

const InitNetworkScript = `#!/bin/sh

: "${ALFS_VM_GATEWAY_IP:?ALFS_VM_GATEWAY_IP is not set}"
: "${ALFS_VM_IP:?ALFS_VM_IP is not set}"
: "${ALFS_VM_PREFIX_LEN:?ALFS_VM_PREFIX_LEN is not set}"

ifconfig vtnet0 inet "${ALFS_VM_IP}/${ALFS_VM_PREFIX_LEN}"
route add default "${ALFS_VM_GATEWAY_IP}"
printf 'nameserver %s\n' "${ALFS_VM_GATEWAY_IP}" > /etc/resolv.conf
`
