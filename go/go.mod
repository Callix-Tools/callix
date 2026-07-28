module github.com/microsoft/typescript-go/callixbridge

// Имя модуля лежит внутри пути typescript-go намеренно: Go разрешает
// импорт internal/-пакетов по пути импорта, а не по расположению на диске.
// Так мост добирается до internal/project и internal/ls без форка ts-go.

go 1.26

require (
	github.com/microsoft/typescript-go v0.0.0
	golang.org/x/tools v0.47.0
)

require (
	github.com/go-json-experiment/json v0.0.0-20260623181947-01eb4420fa68 // indirect
	github.com/klauspost/cpuid/v2 v2.2.10 // indirect
	github.com/mackerelio/go-osstat v0.2.7 // indirect
	github.com/zeebo/xxh3 v1.1.0 // indirect
	golang.org/x/mod v0.37.0 // indirect
	golang.org/x/sync v0.21.0 // indirect
	golang.org/x/sys v0.46.0 // indirect
	golang.org/x/text v0.38.0 // indirect
)

replace github.com/microsoft/typescript-go => ../.ts-go
