//! Search units for languages other than Rust, Bash and Markdown.
//!
//! `cvb pull` reports a hit as the smallest syntactic unit around it (a function,
//! a type, a test). Rust uses `syn`, Bash and every language here use
//! tree-sitter. A language is a table: which file extensions, which node kinds
//! are units, and which parent nodes belong to a unit (a Python decorator, a C++
//! `template <...>` line, a C `typedef`, a JavaScript `export`).
//!
//! Unlike Bash, a file with syntax errors still yields units: nodes that contain
//! an error are skipped, the rest are used. Only when an error file has no unit
//! around the hit does the caller fall back to a single line, with a warning.

use tree_sitter::{Language, Node, Parser};

/// A node kind that forms a unit.
struct Unit {
    kind: &'static str,
    label: &'static str,
    /// The node counts only when it has a direct child of one of these kinds
    /// (a type mentioned as `struct S *p` has no body and is not a unit).
    needs_child: &'static [&'static str],
}

pub(crate) struct Lang {
    extensions: &'static [&'static str],
    language: fn() -> Language,
    units: &'static [Unit],
    /// A unit that is the direct child of one of these nodes takes the parent's
    /// whole range.
    wrappers: &'static [&'static str],
    /// Call-expression node kinds, each with the field that holds the callee
    /// (`callers:`).
    calls: &'static [(&'static str, &'static str)],
}

const fn unit(kind: &'static str, label: &'static str) -> Unit {
    Unit { kind, label, needs_child: &[] }
}

const fn unit_with(kind: &'static str, label: &'static str, needs_child: &'static [&'static str]) -> Unit {
    Unit { kind, label, needs_child }
}

const CALLS_FUNCTION: &[(&str, &str)] = &[("call_expression", "function")];

const BODY_C: &[&str] = &["field_declaration_list", "enumerator_list"];

static ZIG: Lang = Lang {
    extensions: &["zig"],
    language: || tree_sitter_zig::LANGUAGE.into(),
    units: &[
        unit("function_declaration", "Zig function"),
        unit("test_declaration", "Zig test"),
        unit_with(
            "variable_declaration",
            "Zig type",
            &["struct_declaration", "enum_declaration", "union_declaration", "opaque_declaration"],
        ),
    ],
    wrappers: &[],
    calls: CALLS_FUNCTION,
};

static PYTHON: Lang = Lang {
    extensions: &["py", "pyi"],
    language: || tree_sitter_python::LANGUAGE.into(),
    units: &[
        unit("function_definition", "Python function"),
        unit("class_definition", "Python class"),
    ],
    wrappers: &["decorated_definition"],
    calls: &[("call", "function")],
};

static C: Lang = Lang {
    extensions: &["c", "h"],
    language: || tree_sitter_c::LANGUAGE.into(),
    units: &[
        unit("function_definition", "C function"),
        unit_with("struct_specifier", "C type", BODY_C),
        unit_with("union_specifier", "C type", BODY_C),
        unit_with("enum_specifier", "C type", BODY_C),
    ],
    wrappers: &["type_definition"],
    calls: CALLS_FUNCTION,
};

static CPP: Lang = Lang {
    extensions: &["cc", "cpp", "cxx", "hh", "hpp", "hxx"],
    language: || tree_sitter_cpp::LANGUAGE.into(),
    units: &[
        unit("function_definition", "C++ function"),
        unit_with("class_specifier", "C++ type", BODY_C),
        unit_with("struct_specifier", "C++ type", BODY_C),
        unit_with("union_specifier", "C++ type", BODY_C),
        unit_with("enum_specifier", "C++ type", BODY_C),
    ],
    wrappers: &["template_declaration", "type_definition"],
    calls: CALLS_FUNCTION,
};

/// An arrow function or function expression bound to a name
/// (`const f = () => {}`): the declarator is the unit.
const FUNCTION_VALUES: &[&str] = &["arrow_function", "function_expression", "function"];

static JAVASCRIPT: Lang = Lang {
    extensions: &["js", "mjs", "cjs", "jsx"],
    language: || tree_sitter_javascript::LANGUAGE.into(),
    units: &[
        unit("function_declaration", "JavaScript function"),
        unit("generator_function_declaration", "JavaScript function"),
        unit("method_definition", "JavaScript function"),
        unit_with("variable_declarator", "JavaScript function", FUNCTION_VALUES),
        unit("class_declaration", "JavaScript class"),
    ],
    wrappers: &["export_statement", "lexical_declaration", "variable_declaration"],
    calls: CALLS_FUNCTION,
};

const TYPESCRIPT_UNITS: &[Unit] = &[
    unit("function_declaration", "TypeScript function"),
    unit("generator_function_declaration", "TypeScript function"),
    unit("method_definition", "TypeScript function"),
    unit_with("variable_declarator", "TypeScript function", FUNCTION_VALUES),
    unit("class_declaration", "TypeScript class"),
    unit("abstract_class_declaration", "TypeScript class"),
    unit("interface_declaration", "TypeScript type"),
    unit("type_alias_declaration", "TypeScript type"),
    unit("enum_declaration", "TypeScript type"),
];

const TYPESCRIPT_WRAPPERS: &[&str] = &["export_statement", "lexical_declaration", "variable_declaration"];

static TYPESCRIPT: Lang = Lang {
    extensions: &["ts", "mts", "cts"],
    language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    units: TYPESCRIPT_UNITS,
    wrappers: TYPESCRIPT_WRAPPERS,
    calls: CALLS_FUNCTION,
};

/// `.tsx` needs its own grammar: `<T>` is a type assertion in `.ts` and an
/// element in `.tsx`.
static TSX: Lang = Lang {
    extensions: &["tsx"],
    language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
    units: TYPESCRIPT_UNITS,
    wrappers: TYPESCRIPT_WRAPPERS,
    calls: CALLS_FUNCTION,
};

static GO: Lang = Lang {
    extensions: &["go"],
    language: || tree_sitter_go::LANGUAGE.into(),
    units: &[
        unit("function_declaration", "Go function"),
        unit("method_declaration", "Go function"),
        unit("type_spec", "Go type"),
        unit("type_alias", "Go type"),
    ],
    wrappers: &[],
    calls: CALLS_FUNCTION,
};

static JAVA: Lang = Lang {
    extensions: &["java"],
    language: || tree_sitter_java::LANGUAGE.into(),
    units: &[
        unit("method_declaration", "Java method"),
        unit("constructor_declaration", "Java method"),
        unit("class_declaration", "Java type"),
        unit("interface_declaration", "Java type"),
        unit("enum_declaration", "Java type"),
        unit("record_declaration", "Java type"),
        unit("annotation_type_declaration", "Java type"),
    ],
    wrappers: &[],
    calls: &[("method_invocation", "name")],
};

static LUA: Lang = Lang {
    extensions: &["lua"],
    language: || tree_sitter_lua::LANGUAGE.into(),
    units: &[unit("function_declaration", "Lua function")],
    wrappers: &[],
    calls: &[("function_call", "name")],
};

/// Bash is read by `pull` itself; this table only serves `callers:`, so it is
/// not in `LANGUAGES`.
static BASH: Lang = Lang {
    extensions: &["sh", "bash"],
    language: || tree_sitter_bash::LANGUAGE.into(),
    units: &[unit("function_definition", "Bash function")],
    wrappers: &[],
    calls: &[("command", "name")],
};

static LANGUAGES: [&Lang; 10] = [
    &ZIG,
    &PYTHON,
    &C,
    &CPP,
    &JAVASCRIPT,
    &TYPESCRIPT,
    &TSX,
    &GO,
    &JAVA,
    &LUA,
];

/// The language of `path`, by extension.
pub(crate) fn language_for(path: &str) -> Option<&'static Lang> {
    let extension = path.rsplit_once('.')?.1;
    LANGUAGES
        .iter()
        .copied()
        .find(|lang| lang.extensions.contains(&extension))
}

/// The smallest unit of `source` that contains `hit_line` (1-based), with its
/// label; `Line` when no unit does. `Err` when the file cannot be parsed, or has
/// syntax errors and no unit around the hit.
pub(crate) fn unit_range(
    lang: &Lang,
    source: &str,
    hit_line: usize,
) -> Result<((usize, usize), &'static str), ()> {
    let mut parser = Parser::new();
    parser.set_language(&(lang.language)()).map_err(|_| ())?;
    let tree = parser.parse(source, None).ok_or(())?;
    let root = tree.root_node();

    let mut best: Option<((usize, usize), &'static str)> = None;
    collect(lang, root, hit_line, &mut best);
    match best {
        Some(found) => Ok(found),
        None if root.has_error() => Err(()),
        None => Ok(((hit_line, hit_line), "Line")),
    }
}

/// The language `callers:` uses for `path`: Bash by extension, else the unit
/// languages.
pub(crate) fn callers_language_for(path: &str) -> Option<&'static Lang> {
    let extension = path.rsplit_once('.')?.1;
    if BASH.extensions.contains(&extension) {
        return Some(&BASH);
    }
    language_for(path)
}

/// A call to the searched name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CallSite {
    pub line: usize,
    /// Name of the function around the call, when there is one.
    pub enclosing: Option<String>,
    /// `obj.name(...)`: the receiver is not known, so this is only a candidate.
    pub member: bool,
}

/// Calls to `target` in `source`, by line, plus whether the file has syntax
/// errors (the scan then skips the broken calls and may be incomplete). `None`
/// when the file cannot be parsed at all.
pub(crate) fn callers(lang: &Lang, source: &str, target: &str) -> Option<(Vec<CallSite>, bool)> {
    let mut parser = Parser::new();
    parser.set_language(&(lang.language)()).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    let mut found = Vec::new();
    scan_calls(lang, root, source.as_bytes(), target, None, &mut found);
    found.sort_by_key(|site| site.line);
    Some((found, root.has_error()))
}

/// Whether a unit label names something that can contain calls.
fn is_callable(label: &str) -> bool {
    label.ends_with(" function") || label.ends_with(" method")
}

/// The name of a function-like node: its `name` field, or the name inside its
/// `declarator` chain (C and C++).
fn unit_name(node: Node, source: &[u8]) -> Option<String> {
    let mut current = node;
    loop {
        if let Some(name) = current.child_by_field_name("name") {
            return name.utf8_text(source).ok().map(str::to_string);
        }
        match current.child_by_field_name("declarator") {
            Some(next)
                if matches!(next.kind(), "identifier" | "field_identifier" | "qualified_identifier") =>
            {
                return next.utf8_text(source).ok().map(str::to_string);
            }
            Some(next) => current = next,
            None => return None,
        }
    }
}

/// The called name of a callee expression, and whether it was reached through a
/// receiver or namespace member (`a.f`, `a->f`, `a:f`).
fn callee_name<'s>(node: Node, source: &'s [u8]) -> Option<(&'s str, bool)> {
    match node.kind() {
        "identifier" => Some((node.utf8_text(source).ok()?, false)),
        "command_name" => {
            let word = node.named_child(0).filter(|child| child.kind() == "word")?;
            Some((word.utf8_text(source).ok()?, false))
        }
        "member_expression" | "attribute" | "selector_expression" | "field_expression"
        | "dot_index_expression" | "method_index_expression" => {
            let field = ["property", "attribute", "field", "member", "method"]
                .iter()
                .find_map(|name| node.child_by_field_name(name))?;
            Some((field.utf8_text(source).ok()?, true))
        }
        // `ns::f(...)` and `f<T>(...)` call `f`.
        "qualified_identifier" | "template_function" => {
            callee_name(node.child_by_field_name("name")?, source)
        }
        _ => None,
    }
}

fn scan_calls(
    lang: &Lang,
    node: Node,
    source: &[u8],
    target: &str,
    enclosing: Option<&str>,
    found: &mut Vec<CallSite>,
) {
    let mut inner = None;
    if !node.has_error() {
        if let Some(spec) = unit_of(lang, node)
            && is_callable(spec.label)
        {
            inner = unit_name(node, source);
        }
        if let Some((_, field)) = lang.calls.iter().find(|(kind, _)| *kind == node.kind())
            && let Some(callee) = node.child_by_field_name(field)
            && let Some((name, mut member)) = callee_name(callee, source)
            && name == target
        {
            // Java names the method directly; a receiver makes it a candidate.
            member |= node.child_by_field_name("object").is_some();
            found.push(CallSite {
                line: node.start_position().row + 1,
                enclosing: enclosing.map(str::to_string),
                member,
            });
        }
    }
    let enclosing = inner.as_deref().or(enclosing);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_calls(lang, child, source, target, enclosing, found);
    }
}

fn unit_of<'a>(lang: &'a Lang, node: Node) -> Option<&'a Unit> {
    let spec = lang.units.iter().find(|u| u.kind == node.kind())?;
    if spec.needs_child.is_empty() {
        return Some(spec);
    }
    let mut cursor = node.walk();
    let has = node
        .children(&mut cursor)
        .any(|child| spec.needs_child.contains(&child.kind()));
    has.then_some(spec)
}

fn collect(
    lang: &Lang,
    node: Node,
    hit_line: usize,
    best: &mut Option<((usize, usize), &'static str)>,
) {
    let start = node.start_position().row + 1;
    let end = node.end_position().row + 1;
    // A wrapper (decorator, `template <...>`) spans lines its child does not, so
    // the child of a wrapper is always looked at; anything else is skipped when
    // it does not hold the line.
    let holds = start <= hit_line && hit_line <= end;
    let under_wrapper = node.parent().is_some_and(|p| lang.wrappers.contains(&p.kind()));
    if !holds && !under_wrapper {
        return;
    }
    if !node.has_error()
        && let Some(spec) = unit_of(lang, node)
    {
        // Wrappers can nest (`export const f = ...` is an export around a
        // declaration around the declarator), so climb while they continue.
        let mut outer = node;
        while let Some(parent) = outer.parent()
            && lang.wrappers.contains(&parent.kind())
            && !parent.has_error()
        {
            outer = parent;
        }
        let range = (outer.start_position().row + 1, outer.end_position().row + 1);
        // Units nest and the walk goes outside in, so the last one found is
        // the innermost that holds the line.
        *best = Some((range, spec.label));
    }
    recurse(lang, node, hit_line, best);
}

fn recurse(
    lang: &Lang,
    node: Node,
    hit_line: usize,
    best: &mut Option<((usize, usize), &'static str)>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(lang, child, hit_line, best);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(path: &str, source: &str, line: usize) -> Result<((usize, usize), &'static str), ()> {
        unit_range(language_for(path).expect("language"), source, line)
    }

    #[test]
    fn languages_are_chosen_by_extension() {
        assert!(language_for("src/main.zig").is_some());
        assert!(language_for("a/b.py").is_some());
        assert!(language_for("a/b.pyi").is_some());
        assert!(language_for("x.c").is_some());
        assert!(language_for("x.h").is_some());
        for name in ["x.cc", "x.cpp", "x.cxx", "x.hpp", "x.hh", "x.hxx"] {
            assert!(language_for(name).is_some(), "{name}");
        }
        for name in [
            "x.js", "x.mjs", "x.cjs", "x.jsx", "x.ts", "x.mts", "x.cts", "x.tsx", "x.go", "x.java",
            "x.lua",
        ] {
            assert!(language_for(name).is_some(), "{name}");
        }
        assert!(language_for("x.rs").is_none());
        assert!(language_for("x.json").is_none());
        assert!(language_for("Makefile").is_none());
        assert!(language_for("zig").is_none());
        // Only the last extension counts, and dots in directory names do not.
        assert!(language_for("my.tool/x.zig").is_some());
        assert!(language_for("a.b.py").is_some());
        assert!(language_for("a.py.txt").is_none());
    }

    const ZIG_SRC: &str = "const std = @import(\"std\");\nconst Foo = struct {\n    a: u32,\n    pub fn init() Foo {\n        return .{ .a = 1 };\n    }\n};\npub fn main() void {}\ntest \"x\" {\n    try std.testing.expect(true);\n}\nconst E = enum { a, b };\n";

    #[test]
    fn zig_function_test_and_type_units() {
        // A method is its own unit, not the struct around it.
        assert_eq!(range("a.zig", ZIG_SRC, 5), Ok(((4, 6), "Zig function")));
        // A field belongs to the struct.
        assert_eq!(range("a.zig", ZIG_SRC, 3), Ok(((2, 7), "Zig type")));
        assert_eq!(range("a.zig", ZIG_SRC, 8), Ok(((8, 8), "Zig function")));
        assert_eq!(range("a.zig", ZIG_SRC, 10), Ok(((9, 11), "Zig test")));
        assert_eq!(range("a.zig", ZIG_SRC, 12), Ok(((12, 12), "Zig type")));
        // A plain constant is no unit.
        assert_eq!(range("a.zig", ZIG_SRC, 1), Ok(((1, 1), "Line")));
    }

    const PY_SRC: &str = "@dec\ndef f(x):\n    return x\n\nclass A:\n    y = 1\n    def m(self):\n        pass\n";

    #[test]
    fn python_decorators_belong_to_the_function_and_methods_to_themselves() {
        assert_eq!(range("a.py", PY_SRC, 1), Ok(((1, 3), "Python function")));
        assert_eq!(range("a.py", PY_SRC, 3), Ok(((1, 3), "Python function")));
        assert_eq!(range("a.py", PY_SRC, 8), Ok(((7, 8), "Python function")));
        assert_eq!(range("a.py", PY_SRC, 6), Ok(((5, 8), "Python class")));
        assert_eq!(range("a.py", PY_SRC, 4), Ok(((4, 4), "Line")));
    }

    const C_SRC: &str = "struct S { int a; };\nstatic int f(int x) {\n  return x;\n}\nenum E { A, B };\ntypedef struct {\n  int b;\n} T;\nstruct S *p;\n";

    #[test]
    fn c_functions_and_types_with_bodies() {
        assert_eq!(range("a.c", C_SRC, 1), Ok(((1, 1), "C type")));
        assert_eq!(range("a.c", C_SRC, 3), Ok(((2, 4), "C function")));
        assert_eq!(range("a.c", C_SRC, 5), Ok(((5, 5), "C type")));
        // A typedef takes the whole statement, including its name.
        assert_eq!(range("a.c", C_SRC, 7), Ok(((6, 8), "C type")));
        // A struct used only as a type is not a unit.
        assert_eq!(range("a.c", C_SRC, 9), Ok(((9, 9), "Line")));
    }

    const CPP_SRC: &str = "namespace n {\ntemplate <class T>\nT g(T t) { return t; }\nclass C {\npublic:\n  int m() { return 1; }\n};\n}\nint C::h() { return 2; }\n";

    #[test]
    fn cpp_templates_classes_methods_and_out_of_line_definitions() {
        // The template line belongs to the function.
        assert_eq!(range("a.cpp", CPP_SRC, 2), Ok(((2, 3), "C++ function")));
        assert_eq!(range("a.cpp", CPP_SRC, 3), Ok(((2, 3), "C++ function")));
        assert_eq!(range("a.cpp", CPP_SRC, 6), Ok(((6, 6), "C++ function")));
        assert_eq!(range("a.cpp", CPP_SRC, 5), Ok(((4, 7), "C++ type")));
        assert_eq!(range("a.cpp", CPP_SRC, 9), Ok(((9, 9), "C++ function")));
        // A namespace is not a unit.
        assert_eq!(range("a.cpp", CPP_SRC, 1), Ok(((1, 1), "Line")));
    }

    const JS_SRC: &str = "export function a() {\n  return 1;\n}\nconst b = (x) => {\n  return x;\n};\nexport const c = async () => 2;\nclass K {\n  static s = 1;\n  m() {\n    return 3;\n  }\n}\nconst plain = 5;\nfunction* g() {}\n";

    #[test]
    fn javascript_functions_arrows_classes_and_methods() {
        // `export` belongs to the function.
        assert_eq!(range("a.js", JS_SRC, 2), Ok(((1, 3), "JavaScript function")));
        assert_eq!(range("a.js", JS_SRC, 1), Ok(((1, 3), "JavaScript function")));
        // A named arrow function is a function, with its `const`.
        assert_eq!(range("a.js", JS_SRC, 5), Ok(((4, 6), "JavaScript function")));
        assert_eq!(range("a.mjs", JS_SRC, 7), Ok(((7, 7), "JavaScript function")));
        // A method is its own unit, a field belongs to the class.
        assert_eq!(range("a.jsx", JS_SRC, 11), Ok(((10, 12), "JavaScript function")));
        assert_eq!(range("a.cjs", JS_SRC, 9), Ok(((8, 13), "JavaScript class")));
        assert_eq!(range("a.js", JS_SRC, 14), Ok(((14, 14), "Line")));
        assert_eq!(range("a.js", JS_SRC, 15), Ok(((15, 15), "JavaScript function")));
    }

    const TS_SRC: &str = "export interface I {\n  a: number;\n}\ntype T = { b: string };\nenum E { A, B }\nexport abstract class C {\n  abstract f(): void;\n  g(): number {\n    return 1;\n  }\n}\nexport const h = (x: number): number => x;\nfunction k<T>(t: T): T { return t; }\n";

    #[test]
    fn typescript_types_classes_and_functions() {
        assert_eq!(range("a.ts", TS_SRC, 2), Ok(((1, 3), "TypeScript type")));
        assert_eq!(range("a.ts", TS_SRC, 4), Ok(((4, 4), "TypeScript type")));
        assert_eq!(range("a.mts", TS_SRC, 5), Ok(((5, 5), "TypeScript type")));
        assert_eq!(range("a.cts", TS_SRC, 9), Ok(((8, 10), "TypeScript function")));
        assert_eq!(range("a.ts", TS_SRC, 7), Ok(((6, 11), "TypeScript class")));
        assert_eq!(range("a.ts", TS_SRC, 12), Ok(((12, 12), "TypeScript function")));
        assert_eq!(range("a.ts", TS_SRC, 13), Ok(((13, 13), "TypeScript function")));
    }

    #[test]
    fn tsx_reads_elements_that_plain_typescript_would_not() {
        let src = "export function App() {\n  return <div>{1}</div>;\n}\nconst B = () => <b>x</b>;\n";
        assert_eq!(range("a.tsx", src, 2), Ok(((1, 3), "TypeScript function")));
        assert_eq!(range("a.tsx", src, 4), Ok(((4, 4), "TypeScript function")));
    }

    const GO_SRC: &str = "package p\n\ntype S struct {\n\tA int\n}\n\ntype (\n\tX int\n\tY string\n)\n\nfunc (s S) M() int {\n\treturn 1\n}\n\nfunc F() {}\n";

    #[test]
    fn go_functions_methods_and_types() {
        assert_eq!(range("a.go", GO_SRC, 4), Ok(((3, 5), "Go type")));
        // In a group, each type is its own unit.
        assert_eq!(range("a.go", GO_SRC, 9), Ok(((9, 9), "Go type")));
        assert_eq!(range("a.go", GO_SRC, 13), Ok(((12, 14), "Go function")));
        assert_eq!(range("a.go", GO_SRC, 16), Ok(((16, 16), "Go function")));
        assert_eq!(range("a.go", GO_SRC, 1), Ok(((1, 1), "Line")));
    }

    const JAVA_SRC: &str = "package p;\n\n@Deprecated\npublic class A {\n  int f;\n  A() {}\n  @Override\n  public String toString() {\n    return \"\";\n  }\n  interface Inner {\n    void x();\n  }\n}\nenum E { X }\nrecord R(int a) {}\n";

    #[test]
    fn java_types_methods_and_constructors() {
        // An annotation belongs to its declaration.
        assert_eq!(range("A.java", JAVA_SRC, 3), Ok(((3, 14), "Java type")));
        assert_eq!(range("A.java", JAVA_SRC, 5), Ok(((3, 14), "Java type")));
        assert_eq!(range("A.java", JAVA_SRC, 6), Ok(((6, 6), "Java method")));
        assert_eq!(range("A.java", JAVA_SRC, 7), Ok(((7, 10), "Java method")));
        assert_eq!(range("A.java", JAVA_SRC, 9), Ok(((7, 10), "Java method")));
        assert_eq!(range("A.java", JAVA_SRC, 11), Ok(((11, 13), "Java type")));
        assert_eq!(range("A.java", JAVA_SRC, 12), Ok(((12, 12), "Java method")));
        assert_eq!(range("A.java", JAVA_SRC, 15), Ok(((15, 15), "Java type")));
        assert_eq!(range("A.java", JAVA_SRC, 16), Ok(((16, 16), "Java type")));
        assert_eq!(range("A.java", JAVA_SRC, 1), Ok(((1, 1), "Line")));
    }

    const LUA_SRC: &str = "local M = {}\n\nfunction M.f(x)\n  return x\nend\n\nlocal function g()\nend\n\nreturn M\n";

    #[test]
    fn lua_function_declarations() {
        assert_eq!(range("a.lua", LUA_SRC, 4), Ok(((3, 5), "Lua function")));
        assert_eq!(range("a.lua", LUA_SRC, 7), Ok(((7, 8), "Lua function")));
        assert_eq!(range("a.lua", LUA_SRC, 1), Ok(((1, 1), "Line")));
        assert_eq!(range("a.lua", LUA_SRC, 10), Ok(((10, 10), "Line")));
    }

    #[test]
    fn nested_wrappers_all_belong_to_the_unit() {
        let src = "template <class T>\ntemplate <class U>\nvoid f(T, U) {}\n";
        assert_eq!(range("a.cpp", src, 1), Ok(((1, 3), "C++ function")));
    }

    #[test]
    fn a_broken_javascript_function_does_not_hide_its_neighbours() {
        let src = "function good() {\n  return 1;\n}\nfoo(;\nfunction fine() {\n  return 2;\n}\n";
        assert_eq!(range("a.js", src, 2), Ok(((1, 3), "JavaScript function")));
        assert_eq!(range("a.js", src, 6), Ok(((5, 7), "JavaScript function")));
        // The broken statement has no unit: the caller falls back to lines.
        assert_eq!(range("a.js", src, 4), Err(()));
    }

    /// (line, enclosing function, through a receiver)
    fn sites(path: &str, source: &str, target: &str) -> Vec<(usize, Option<String>, bool)> {
        let lang = callers_language_for(path).expect("language");
        callers(lang, source, target)
            .expect("parses")
            .0
            .into_iter()
            .map(|site| (site.line, site.enclosing, site.member))
            .collect()
    }

    fn at(line: usize, function: &str, member: bool) -> (usize, Option<String>, bool) {
        (line, Some(function.to_string()), member)
    }

    #[test]
    fn python_and_javascript_calls() {
        let py = "def f():\n    run()\n    a.run(1)\n    runner()\nrun()\n";
        assert_eq!(
            sites("a.py", py, "run"),
            vec![at(2, "f", false), at(3, "f", true), (5, None, false)]
        );
        let js = "function f() {\n  run();\n  a?.run();\n  const g = () => run();\n}\nclass K {\n  m() { return run(); }\n}\n";
        assert_eq!(
            sites("a.js", js, "run"),
            vec![at(2, "f", false), at(3, "f", true), at(4, "g", false), at(7, "m", false)]
        );
    }

    #[test]
    fn typescript_generic_calls_and_tsx() {
        let ts = "function f() {\n  run<number>(1);\n  a.run();\n}\n";
        assert_eq!(sites("a.ts", ts, "run"), vec![at(2, "f", false), at(3, "f", true)]);
        let tsx = "export function App() {\n  return <div>{run()}</div>;\n}\n";
        assert_eq!(sites("a.tsx", tsx, "run"), vec![at(2, "App", false)]);
    }

    #[test]
    fn go_calls_and_methods() {
        let src = "package p\n\nfunc f() {\n\trun()\n\tpkg.run(1)\n}\n\nfunc (s S) m() {\n\trun()\n}\n";
        assert_eq!(
            sites("a.go", src, "run"),
            vec![at(4, "f", false), at(5, "f", true), at(9, "m", false)]
        );
    }

    #[test]
    fn java_calls_with_and_without_a_receiver() {
        let src = "class A {\n  void f() {\n    run();\n    this.run();\n    new Run();\n  }\n  A() { run(); }\n}\n";
        assert_eq!(
            sites("A.java", src, "run"),
            vec![at(3, "f", false), at(4, "f", true), at(7, "A", false)]
        );
    }

    #[test]
    fn lua_zig_and_bash_calls() {
        let lua = "local function f()\n  run()\n  a.run()\n  a:run()\nend\n";
        assert_eq!(
            sites("a.lua", lua, "run"),
            vec![at(2, "f", false), at(3, "f", true), at(4, "f", true)]
        );
        let zig = "pub fn m() void {\n    run();\n    a.run(1);\n}\n";
        assert_eq!(sites("a.zig", zig, "run"), vec![at(2, "m", false), at(3, "m", true)]);
        let sh = "f() {\n  run a b\n  x=$(run c)\n  runner\n}\nrun\n";
        assert_eq!(
            sites("a.sh", sh, "run"),
            vec![at(2, "f", false), at(3, "f", false), (6, None, false)]
        );
        assert_eq!(sites("a.bash", sh, "runner"), vec![at(4, "f", false)]);
    }

    #[test]
    fn c_and_cpp_calls_name_the_function_around_them() {
        let c = "int f(void) {\n  run();\n  s.run(1);\n  p->run(2);\n  return 0;\n}\n";
        assert_eq!(
            sites("a.c", c, "run"),
            vec![at(2, "f", false), at(3, "f", true), at(4, "f", true)]
        );
        let cpp = "int C::h() {\n  run();\n  ns::run();\n  run<int>(1);\n  a.run();\n}\n";
        assert_eq!(
            sites("a.cpp", cpp, "run"),
            vec![at(2, "C::h", false), at(3, "C::h", false), at(4, "C::h", false), at(5, "C::h", true)]
        );
    }

    #[test]
    fn mentions_in_comments_and_strings_are_not_calls() {
        let src = "def f():\n    # run()\n    s = \"run()\"\n    return run\n";
        assert!(sites("a.py", src, "run").is_empty());
    }

    #[test]
    fn a_broken_file_still_reports_the_calls_that_parsed_and_says_it_is_broken() {
        let src = "def good():\n    run()\n\ndef bad(:\n    run(\n";
        let lang = callers_language_for("a.py").unwrap();
        let (found, broken) = callers(lang, src, "run").unwrap();
        assert!(broken);
        assert!(found.iter().any(|site| site.line == 2 && site.enclosing.as_deref() == Some("good")), "{found:?}");
        let (_, broken) = callers(lang, "def f():\n    run()\n", "run").unwrap();
        assert!(!broken);
    }

    #[test]
    fn callable_labels_are_functions_and_methods_only() {
        for lang in LANGUAGES.iter().copied().chain([&BASH]) {
            for unit in lang.units {
                let callable = is_callable(unit.label);
                let expected = !(unit.label.ends_with(" type")
                    || unit.label.ends_with(" class")
                    || unit.label.ends_with(" test"));
                assert_eq!(callable, expected, "{}", unit.label);
            }
        }
    }

    #[test]
    fn a_broken_function_is_skipped_but_its_neighbours_still_count() {
        let src = "def good():\n    return 1\n\ndef bad(:\n    return (\n\ndef also_good():\n    return 2\n";
        assert_eq!(range("a.py", src, 2), Ok(((1, 2), "Python function")));
        assert_eq!(range("a.py", src, 8), Ok(((7, 8), "Python function")));
        // No unit around a line of the broken part: the caller falls back to lines.
        assert_eq!(range("a.py", src, 5), Err(()));
    }

    #[test]
    fn a_line_outside_every_unit_of_a_clean_file_is_a_line() {
        assert_eq!(range("a.py", "import os\n\nX = 1\n", 3), Ok(((3, 3), "Line")));
    }
}
