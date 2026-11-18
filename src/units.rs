//! Search units for languages other than Rust, Bash and Markdown.
//!
//! `cvb pull` reports a hit as the smallest syntactic unit around it (a function,
//! a type, a test). Rust uses `syn`, Bash and every language here use
//! tree-sitter. A language is a table: which file extensions, which node kinds
//! are units, and which parent nodes belong to a unit (a Python decorator, a C++
//! `template <...>` line, a C `typedef`).
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
}

const fn unit(kind: &'static str, label: &'static str) -> Unit {
    Unit { kind, label, needs_child: &[] }
}

const fn unit_with(kind: &'static str, label: &'static str, needs_child: &'static [&'static str]) -> Unit {
    Unit { kind, label, needs_child }
}

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
};

static PYTHON: Lang = Lang {
    extensions: &["py", "pyi"],
    language: || tree_sitter_python::LANGUAGE.into(),
    units: &[
        unit("function_definition", "Python function"),
        unit("class_definition", "Python class"),
    ],
    wrappers: &["decorated_definition"],
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
};

static LANGUAGES: [&Lang; 4] = [&ZIG, &PYTHON, &C, &CPP];

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
        let mut outer = node;
        if let Some(parent) = node.parent()
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
        assert!(language_for("x.rs").is_none());
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
