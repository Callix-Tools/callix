// The bridge to typescript-go: brings the language service up and answers
// goto-definition. Built as a c-archive and linked into callix's native
// module, so tsgo is not needed as a separate process.
package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"fmt"
	"sync"
	"time"
	"unsafe"

	"github.com/microsoft/typescript-go/internal/bundled"
	"github.com/microsoft/typescript-go/internal/diagnostics"
	"github.com/microsoft/typescript-go/internal/locale"
	"github.com/microsoft/typescript-go/internal/lsp/lsproto"
	"github.com/microsoft/typescript-go/internal/project"
	"github.com/microsoft/typescript-go/internal/vfs/osvfs"
)

// nopClient stands in for an LSP client: file watching, diagnostics, and
// telemetry are all pointless for a one-shot analysis.
type nopClient struct{}

func (nopClient) WatchFiles(context.Context, project.WatcherID, []*lsproto.FileSystemWatcher) error {
	return nil
}
func (nopClient) UnwatchFiles(context.Context, project.WatcherID) error { return nil }
func (nopClient) RefreshDiagnostics(context.Context) error             { return nil }
func (nopClient) PublishDiagnostics(context.Context, *lsproto.PublishDiagnosticsParams) error {
	return nil
}
func (nopClient) RefreshInlayHints(context.Context) error                     { return nil }
func (nopClient) RefreshCodeLens(context.Context) error                       { return nil }
func (nopClient) ProgressStart(*diagnostics.Message, ...any)                  {}
func (nopClient) ProgressFinish(*diagnostics.Message, ...any)                 {}
func (nopClient) SendTelemetry(context.Context, lsproto.TelemetryEvent) error { return nil }
func (nopClient) IsActive() bool                                              { return true }
func (nopClient) SetLocale(string)                                            {}
func (nopClient) GetLocale() locale.Locale                                    { return locale.Default }

type session struct {
	inner *project.Session
	ctx   context.Context
}

// Go pointers cannot be handed to C, so a numeric handle goes out instead.
var (
	mu       sync.Mutex
	sessions = map[int]*session{}
	nextID   = 1
)

//export callix_ts_open
func callix_ts_open(root *C.char) C.int {
	dir := C.GoString(root)
	ctx := lsproto.WithClientCapabilities(context.Background(), &lsproto.ResolvedClientCapabilities{})

	inner := project.NewSession(&project.SessionInit{
		BackgroundCtx: ctx,
		Options: &project.SessionOptions{
			CurrentDirectory:   dir,
			DefaultLibraryPath: bundled.LibPath(),
			// UTF-8: our columns are byte offsets, as tree-sitter reports them.
			PositionEncoding: lsproto.PositionEncodingKindUTF8,
			WatchEnabled:     false,
			LoggingEnabled:   false,
			DebounceDelay:    time.Millisecond,
		},
		FS:     bundled.WrapFS(osvfs.FS()),
		Client: nopClient{},
	})

	mu.Lock()
	defer mu.Unlock()
	id := nextID
	nextID++
	sessions[id] = &session{inner: inner, ctx: ctx}
	return C.int(id)
}

//export callix_ts_close
func callix_ts_close(handle C.int) {
	mu.Lock()
	defer mu.Unlock()
	delete(sessions, int(handle))
}

//export callix_ts_free
func callix_ts_free(ptr *C.char) {
	C.free(unsafe.Pointer(ptr))
}

// callix_ts_definition answers with "path\tline\tcolumn" or an empty string
// when there is no definition. Input is 1-based, LSP is 0-based.
//
//export callix_ts_definition
func callix_ts_definition(handle C.int, file *C.char, line C.uint, col C.uint) *C.char {
	mu.Lock()
	s := sessions[int(handle)]
	mu.Unlock()
	if s == nil {
		return C.CString("")
	}

	uri := lsproto.DocumentUri("file://" + C.GoString(file))
	position := lsproto.Position{Line: uint32(line) - 1, Character: uint32(col) - 1}

	languageService, err := s.inner.GetLanguageService(s.ctx, uri)
	if err != nil || languageService == nil {
		return C.CString("")
	}
	response, err := languageService.ProvideDefinition(s.ctx, uri, position)
	if err != nil {
		return C.CString("")
	}
	return C.CString(firstLocation(response))
}

func firstLocation(response lsproto.DefinitionResponse) string {
	format := func(uri lsproto.DocumentUri, r lsproto.Range) string {
		return fmt.Sprintf("%s\t%d\t%d", uri.FileName(), r.Start.Line+1, r.Start.Character+1)
	}
	switch {
	case response.Location != nil:
		return format(response.Location.Uri, response.Location.Range)
	case response.Locations != nil && len(*response.Locations) > 0:
		first := (*response.Locations)[0]
		return format(first.Uri, first.Range)
	case response.DefinitionLinks != nil && len(*response.DefinitionLinks) > 0:
		first := (*response.DefinitionLinks)[0]
		return format(first.TargetUri, first.TargetSelectionRange)
	}
	return ""
}

func main() {}
