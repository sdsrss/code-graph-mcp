FILES = {
 'go.mod': 'module example.com/app\n',
 'lock/mutex.go': 'package lock\n\ntype Mutex struct{}\n\nfunc (m *Mutex) Lock() {}\n',
 'cache/cache.go': 'package cache\n\nimport "sync"\n\ntype Cache struct{ sync.Mutex }\n\nfunc (c *Cache) Get() { c.Lock() }\n',
}
