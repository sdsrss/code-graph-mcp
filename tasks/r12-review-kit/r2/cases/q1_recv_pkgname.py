FILES = {
 'go.mod': 'module example.com/app\n',
 'config/config.go': 'package config\n\nfunc Validate() {}\n',
 'svc/svc.go': '''package svc

import "example.com/app/config"

type Svc struct{}

func (s *Svc) Validate() {}

// receiver named like an imported package
func (config *Svc) Run() {
\tconfig.Validate()
}

func (s *Svc) Use() { config.Validate() }
''',
}
