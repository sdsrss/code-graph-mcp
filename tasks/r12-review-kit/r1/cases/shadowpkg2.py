FILES = {
 'go.mod': 'module example.com/app\n',
 'config/config.go': 'package config\n\ntype Config struct{}\n\nfunc Load() *Config { return nil }\nfunc (c *Config) Validate() error { return nil }\n',
 'user/user.go': 'package user\n\nfunc Get(id int) (int, error) { return 0, nil }\n',
 'main.go': '''package main

import (
\t"example.com/app/config"
\t"example.com/app/user"
)

func run() error {
\tconfig := config.Load()
\treturn config.Validate()
}

func lookup(id int) error {
\tuser, err := user.Get(id)
\t_ = user
\treturn err
}
''',
}
