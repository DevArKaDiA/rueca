// Package store keeps values.
package store

// Repo reads values.
type Repo interface {
	Get(key string) (string, bool)
}

// Store is an in-memory Repo (implicitly).
type Store struct {
	items map[string]string
}

// New returns an empty store.
func New() *Store { return &Store{items: map[string]string{}} }

// Get returns the value of key.
func (s *Store) Get(key string) (string, bool) {
	v, ok := s.items[key]
	return v, ok
}

// Cache is a generic cache.
type Cache[K comparable, V any] struct {
	items map[K]V
}

// Put stores v under k.
func (c *Cache[K, V]) Put(k K, v V) { c.items[k] = v }

// Map applies f to every element.
func Map[T, U any](xs []T, f func(T) U) []U {
	out := make([]U, 0, len(xs))
	for _, x := range xs {
		out = append(out, f(x))
	}
	return out
}
