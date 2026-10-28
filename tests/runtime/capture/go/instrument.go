// instrument rewrites the Go sources of a module copy so that every function
// and function literal records (caller, callee) on entry. Line numbers are
// preserved: the hook goes on the line of the opening brace, the import on the
// line of the package clause.
package main

import (
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

const hookDir = "internal/rttrace"

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: instrument <module root> <module path>")
		os.Exit(2)
	}
	root, module := os.Args[1], os.Args[2]
	hookImport := module + "/" + hookDir
	testPkgs := map[string]string{} // dir -> package name of a _test.go file
	hasMain := map[string]bool{}
	files := 0
	err := filepath.WalkDir(root, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(root, path)
		if d.IsDir() {
			base := d.Name()
			if rel != "." && (base == "testdata" || base == "vendor" || strings.HasPrefix(base, ".") || strings.HasPrefix(base, "_")) {
				return filepath.SkipDir
			}
			if filepath.ToSlash(rel) == hookDir {
				return filepath.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(path, ".go") {
			return nil
		}
		n, pkg, tm, err := rewrite(path, hookImport)
		if err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
		files += n
		dir := filepath.Dir(path)
		if strings.HasSuffix(path, "_test.go") {
			if _, ok := testPkgs[dir]; !ok || !strings.HasSuffix(pkg, "_test") {
				testPkgs[dir] = pkg
			}
			hasMain[dir] = hasMain[dir] || tm
		}
		return nil
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	dirs := make([]string, 0, len(testPkgs))
	for dir := range testPkgs {
		dirs = append(dirs, dir)
	}
	sort.Strings(dirs)
	for _, dir := range dirs {
		if hasMain[dir] {
			fmt.Fprintf(os.Stderr, "%s already has a TestMain: add rttrace.Dump() to it\n", dir)
			os.Exit(1)
		}
		src := fmt.Sprintf("package %s\n\nimport (\n\t\"os\"\n\t\"testing\"\n\n\t__rt %q\n)\n\n"+
			"func TestMain(m *testing.M) {\n\tcode := m.Run()\n\t__rt.Dump()\n\tos.Exit(code)\n}\n",
			testPkgs[dir], hookImport)
		if err := os.WriteFile(filepath.Join(dir, "zz_rttrace_main_test.go"), []byte(src), 0o644); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
	}
	fmt.Fprintf(os.Stderr, "instrumented %d functions, %d test packages\n", files, len(dirs))
}

func rewrite(path, hookImport string) (int, string, bool, error) {
	src, err := os.ReadFile(path)
	if err != nil {
		return 0, "", false, err
	}
	fset := token.NewFileSet()
	file, err := parser.ParseFile(fset, path, src, parser.SkipObjectResolution)
	if err != nil {
		return 0, "", false, err
	}
	var offsets []int
	testMain := false
	ast.Inspect(file, func(n ast.Node) bool {
		switch fn := n.(type) {
		case *ast.FuncDecl:
			if fn.Body != nil {
				offsets = append(offsets, fset.Position(fn.Body.Lbrace).Offset+1)
			}
			if fn.Recv == nil && fn.Name.Name == "TestMain" {
				testMain = true
			}
		case *ast.FuncLit:
			offsets = append(offsets, fset.Position(fn.Body.Lbrace).Offset+1)
		}
		return true
	})
	if len(offsets) == 0 {
		return 0, file.Name.Name, testMain, nil
	}
	offsets = append(offsets, fset.Position(file.Name.End()).Offset)
	sort.Ints(offsets)
	nameEnd := fset.Position(file.Name.End()).Offset
	var out strings.Builder
	last := 0
	for _, off := range offsets {
		out.Write(src[last:off])
		if off == nameEnd {
			fmt.Fprintf(&out, "; import __rt %q", hookImport)
		} else {
			out.WriteString("__rt.E();")
		}
		last = off
	}
	out.Write(src[last:])
	return len(offsets) - 1, file.Name.Name, testMain, os.WriteFile(path, []byte(out.String()), 0o644)
}
