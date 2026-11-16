FILES = {
 'go.mod': 'module example.com/app\n',
 'config/config.go': 'package config\n\ntype Config struct{}\n\nfunc Validate() error { return nil }\nfunc (c *Config) Validate() error { return nil }\n',
 'main.go': '''package main

import "example.com/app/config"

func literalParam() {
\tf := func(config *config.Config) { config.Validate() }
\tf(nil)
}

func ranged(cs []*config.Config) {
\tfor _, config := range cs {
\t\tconfig.Validate()
\t}
}

func namedResult2() (config *config.Config, err error) {
\terr = config.Validate()
\treturn
}
''',
}
