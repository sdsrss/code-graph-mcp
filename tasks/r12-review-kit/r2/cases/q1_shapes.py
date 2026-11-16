CFG = 'package config\n\ntype Config struct{}\n\nfunc Load() *Config { return nil }\nfunc Validate() error { return nil }\nfunc (c *Config) Validate() error { return nil }\nfunc (c *Config) Reload() {}\n'
FILES = {
 'go.mod': 'module example.com/app\n',
 'config/config.go': CFG,
 'main.go': '''package main

import (
\t"example.com/app/config"
\tcfg "example.com/app/config"
)

// named result shadows the package in the whole body: a value call
func namedResult() (config *config.Config) {
\tconfig.Validate()
\treturn
}

// parameter, grouped "a, b T": value calls
func grouped(other, config *cfg.Config) {
\tconfig.Validate()
}

// variadic parameter: value call
func variadic(config ...*cfg.Config) {
\tconfig[0].Validate()
}

// func literal parameter: value call inside the literal
func literalParam() {
\tf := func(config *cfg.Config) { config.Reload() }
\tf(nil)
}

// aliased import shadowed by :=: right side is the package
func aliased() {
\tcfg := cfg.Load()
\tcfg.Reload()
}

// range variable named like the package: value call
func ranged(cs []*cfg.Config) {
\tfor _, config := range cs {
\t\tconfig.Reload()
\t}
}

// type switch alias named like the package
func switched(x interface{}) {
\tswitch config := x.(type) {
\tcase *cfg.Config:
\t\tconfig.Reload()
\t}
}
''',
}
