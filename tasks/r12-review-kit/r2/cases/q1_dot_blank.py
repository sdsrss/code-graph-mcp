FILES = {
 'go.mod': 'module example.com/app\n',
 'config/config.go': 'package config\n\ntype Config struct{}\n\nfunc Load() *Config { return nil }\nfunc Reload() {}\nfunc (c *Config) Reload() {}\n',
 'dot.go': '''package main

import . "example.com/app/config"

func dot() {
\tconfig := Load()
\tconfig.Reload()
}
''',
 'blank.go': '''package main

import _ "example.com/app/config"

type local struct{}

func (l *local) Reload() {}

func blank(l *local) {
\tconfig := l
\tconfig.Reload()
}
''',
}
