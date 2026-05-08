//go:build !darwin

package main

func stampOverrideStat(string) error { return nil }
