// Package money handles amounts.
package money

import "strconv"

// Amount is an amount in cents.
type Amount int64

// String formats the amount.
func (a Amount) String() string { return strconv.FormatInt(int64(a), 10) }

// Parse parses an amount.
func Parse(s string) (Amount, error) {
	n, err := strconv.ParseInt(s, 10, 64)
	return Amount(n), err
}
