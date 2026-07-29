// Package store holds the types the service annotates against.
package store

import "fmt"

// Identifier names a record.
type Identifier = int

// Labeled is embedded by Engine.
type Labeled interface {
	Describe() string
}

// Base carries a field and a method.
type Base struct {
	Label string
}

func (b Base) Describe() string {
	return b.Label
}

// Engine embeds Base and satisfies Labeled.
type Engine struct {
	Base
	Region string
}

func (e Engine) Ping() bool {
	return fmt.Sprint(e.Region) != ""
}
