# the shadow rule across edits of the caller and of the package
FILES = {
 'go.mod': 'module example.com/app\n',
 'config/config.go': 'package config\n\ntype Config struct{}\n\nfunc Load() *Config { return nil }\n',
 'main.go': 'package main\n\nimport "example.com/app/config"\n\nfunc run() {\n\tconfig := config.Load()\n\t_ = config\n}\n',
}
EDITS = [
 {'config/config.go': 'package config\n\ntype Config struct{}\n'},
 {'config/config.go': 'package config\n\ntype Config struct{}\n\nfunc Load() *Config { return nil }\n'},
 {'config/config.go': 'package config\n\ntype Config struct{}\n\nfunc (c *Config) Load() *Config { return nil }\n'},
 {'config/config.go': 'package config\n\ntype Config struct{}\n\nfunc Load() *Config { return nil }\n'},
]
