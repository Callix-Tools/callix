// Мост к go/packages: типизирует проект и отвечает на goto-definition.
// В отличие от ty и typescript-go, стандартная библиотека Go не может
// быть вкомпилирована — её исходники берутся из GOROOT, поэтому этому
// резолверу нужен установленный Go у того, кто анализирует.
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

// fileUnit — файл в контексте одного пакета: с Tests=true один и тот же
// файл входит сразу в несколько пакетов (обычный, тестовый вариант,
// внешний test-пакет), и типовая информация у них разная.
type fileUnit struct {
	pkg    *packages.Package
	syntax *ast.File
}

// goSession держит загруженные пакеты и индекс файлов проекта.
type goSession struct {
	packages []*packages.Package
	// Файл → все его вхождения; перебираются по очереди, пока какое-то
	// не даст определение.
	byFile map[string][]fileUnit
	// Встроенные имена (len, make, append, string, error, …) → позиция в
	// $GOROOT/src/builtin/builtin.go. Своей позиции в исходниках у них
	// нет — они живут в universe scope, — но gopls указывает именно на
	// builtin.go, и без этого каждый вызов len() остаётся неразрешённым.
	builtins map[string]token.Position
}

// loadBuiltins разбирает пакет builtin и запоминает позиции его деклараций.
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
							// Методы встроенных интерфейсов (error.Error)
							// тоже живут в universe scope и своей позиции
							// в пользовательском коде не имеют.
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
		// Без этого тестовые файлы не типизируются, и все места
		// использования в *_test.go остаются неразрешёнными.
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
	// Индексируем только пакеты проекта: зависимости нужны для типов,
	// но запросы приходят по файлам проекта.
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

// callix_go_definition отвечает строкой "путь\tстрока\tколонка" либо
// пустой строкой. Координаты 1-based с обеих сторон.
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
		// Встроенное имя: своей позиции нет, берём из builtin.go.
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

// positionToPos переводит 1-based (строка, колонка) в token.Pos.
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

// identAt возвращает идентификатор, покрывающий позицию.
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

// lookupObject: сначала использование, затем объявление — так место
// использования ведёт к определению, а само определение к себе же.
func lookupObject(info *types.Info, ident *ast.Ident) types.Object {
	if object := info.Uses[ident]; object != nil {
		return object
	}
	return info.Defs[ident]
}
