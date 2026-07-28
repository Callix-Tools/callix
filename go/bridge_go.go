// The bridge to go/packages: type-checks the project and answers
// goto-definition. Unlike ty and typescript-go, Go's standard library cannot
// be compiled in — its sources come from GOROOT, so this resolver needs Go
// installed on the machine doing the analysis.
package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"fmt"
	"go/ast"
	"go/token"
	"go/types"
	"sync"

	"golang.org/x/tools/go/packages"
)

// fileUnit is a file in the context of one package: with Tests=true the same
// file belongs to several packages at once (the normal one, the test variant,
// the external test package), and their type information differs.
type fileUnit struct {
	pkg    *packages.Package
	syntax *ast.File
}

// goSession holds the loaded packages and the project's file index.
type goSession struct {
	packages []*packages.Package
	// File → all of its units; tried in turn until one yields a definition.
	byFile map[string][]fileUnit
	// Builtin names (len, make, append, string, error, …) → their position in
	// $GOROOT/src/builtin/builtin.go. They have no position of their own in
	// the sources — they live in universe scope — but gopls points at
	// builtin.go, and without this every len() call stays unresolved.
	builtins map[string]token.Position
}

// loadBuiltins parses the builtin package and records its declarations'
// positions.
func loadBuiltins(dir string) map[string]token.Position {
	out := map[string]token.Position{}
	cfg := &packages.Config{
		Mode: packages.NeedSyntax | packages.NeedTypes | packages.NeedTypesInfo | packages.NeedFiles,
		Dir:  dir,
	}
	loaded, err := packages.Load(cfg, "builtin")
	if err != nil {
		return out
	}
	for _, pkg := range loaded {
		for _, file := range pkg.Syntax {
			for _, decl := range file.Decls {
				switch d := decl.(type) {
				case *ast.FuncDecl:
					out[d.Name.Name] = pkg.Fset.Position(d.Name.Pos())
				case *ast.GenDecl:
					for _, spec := range d.Specs {
						switch s := spec.(type) {
						case *ast.TypeSpec:
							out[s.Name.Name] = pkg.Fset.Position(s.Name.Pos())
							// Methods of builtin interfaces (error.Error) also
							// live in universe scope and have no position of
							// their own in user code.
							if iface, ok := s.Type.(*ast.InterfaceType); ok && iface.Methods != nil {
								for _, method := range iface.Methods.List {
									for _, name := range method.Names {
										out[name.Name] = pkg.Fset.Position(name.Pos())
									}
								}
							}
						case *ast.ValueSpec:
							for _, name := range s.Names {
								out[name.Name] = pkg.Fset.Position(name.Pos())
							}
						}
					}
				}
			}
		}
	}
	return out
}

var (
	goMu       sync.Mutex
	goSessions = map[int]*goSession{}
	goNextID   = 1
)

//export callix_go_open
func callix_go_open(root *C.char) C.int {
	dir := C.GoString(root)
	cfg := &packages.Config{
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedCompiledGoFiles |
			packages.NeedSyntax | packages.NeedTypes | packages.NeedTypesInfo |
			packages.NeedImports | packages.NeedDeps,
		Dir: dir,
		// Without this, test files are not type-checked and every use-site in
		// *_test.go stays unresolved.
		Tests: true,
	}
	loaded, err := packages.Load(cfg, "./...")
	if err != nil {
		return C.int(-1)
	}

	session := &goSession{
		packages: loaded,
		byFile:   map[string][]fileUnit{},
		builtins: loadBuiltins(dir),
	}
	// Only the project's packages are indexed: dependencies are needed for
	// types, but queries arrive for the project's own files.
	for _, pkg := range loaded {
		for i, file := range pkg.Syntax {
			if i >= len(pkg.CompiledGoFiles) {
				break
			}
			name := pkg.CompiledGoFiles[i]
			session.byFile[name] = append(session.byFile[name], fileUnit{pkg: pkg, syntax: file})
		}
	}

	goMu.Lock()
	defer goMu.Unlock()
	id := goNextID
	goNextID++
	goSessions[id] = session
	return C.int(id)
}

//export callix_go_close
func callix_go_close(handle C.int) {
	goMu.Lock()
	defer goMu.Unlock()
	delete(goSessions, int(handle))
}

// callix_go_definition answers with "path\tline\tcolumn" or an empty string.
// Coordinates are 1-based on both sides.
//
//export callix_go_definition
func callix_go_definition(handle C.int, file *C.char, line C.uint, col C.uint) *C.char {
	goMu.Lock()
	session := goSessions[int(handle)]
	goMu.Unlock()
	if session == nil {
		return C.CString("")
	}

	name := C.GoString(file)
	for _, unit := range session.byFile[name] {
		offset := positionToPos(unit.pkg.Fset, name, int(line), int(col))
		if offset == token.NoPos {
			continue
		}
		ident := identAt(unit.syntax, offset)
		if ident == nil {
			continue
		}
		object := lookupObject(unit.pkg.TypesInfo, ident)
		if object == nil {
			continue
		}
		// A builtin name: no position of its own, so take it from builtin.go.
		if object.Pos() == token.NoPos {
			position, ok := session.builtins[object.Name()]
			if !ok {
				continue
			}
			return C.CString(fmt.Sprintf("%s\t%d\t%d", position.Filename, position.Line, position.Column))
		}
		target := unit.pkg.Fset.Position(object.Pos())
		return C.CString(fmt.Sprintf("%s\t%d\t%d", target.Filename, target.Line, target.Column))
	}
	return C.CString("")
}

// positionToPos converts a 1-based (line, column) into a token.Pos.
func positionToPos(fset *token.FileSet, filename string, line, col int) token.Pos {
	var found *token.File
	fset.Iterate(func(f *token.File) bool {
		if f.Name() == filename {
			found = f
			return false
		}
		return true
	})
	if found == nil || line < 1 || line > found.LineCount() {
		return token.NoPos
	}
	linePos := found.LineStart(line)
	offset := found.Offset(linePos) + col - 1
	if offset < 0 || offset > found.Size() {
		return token.NoPos
	}
	return found.Pos(offset)
}

// identAt returns the identifier covering the position.
func identAt(file *ast.File, pos token.Pos) *ast.Ident {
	var found *ast.Ident
	ast.Inspect(file, func(node ast.Node) bool {
		if node == nil || found != nil {
			return false
		}
		if node.Pos() > pos || node.End() <= pos {
			return false
		}
		if ident, ok := node.(*ast.Ident); ok {
			found = ident
		}
		return true
	})
	return found
}

// lookupObject: uses first, then definitions — so a use-site leads to its
// definition, and a definition leads to itself.
func lookupObject(info *types.Info, ident *ast.Ident) types.Object {
	if object := info.Uses[ident]; object != nil {
		return object
	}
	return info.Defs[ident]
}
