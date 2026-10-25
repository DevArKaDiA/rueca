package util

// Clamp limits v to [lo, hi].
func Clamp(v, lo, hi int) int { return min(max(v, lo), hi) }
