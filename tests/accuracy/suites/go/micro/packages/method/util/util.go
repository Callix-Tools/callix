package util

func Do() { inner() }

func inner() {}

type T struct{}

func (T) Go() {}

func New() *T { return &T{} }
