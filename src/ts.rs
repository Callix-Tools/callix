//! Общие хелперы поверх tree-sitter: обход дерева и перевод позиций в Span.
//!
//! Позиции tree-sitter 0-based, Span — 1-based, конвертация живёт здесь и
//! больше нигде.

use std::collections::HashMap;

use tree_sitter::Node as TsNode;

use crate::span::Span;

/// Текст узла. Битый UTF-8 заменяется, а не роняет разбор.
pub fn text<'a>(node: TsNode<'_>, source: &'a [u8]) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy(&source[node.byte_range()])
}

/// Прямые потомки, включая анонимные (как `node.children` в Python-API).
pub fn children<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// Поиск потомка идёт курсором, без сборки `Vec`: на больших деревьях
/// (типы в TypeScript) эти аллокации заметны в профиле.
pub fn child_of_type<'tree>(node: TsNode<'tree>, kind: &str) -> Option<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| c.kind() == kind)
}

pub fn child_of_types<'tree>(node: TsNode<'tree>, kinds: &[&str]) -> Option<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|c| kinds.contains(&c.kind()))
}

pub fn has_child_of_type(node: TsNode<'_>, kind: &str) -> bool {
    child_of_type(node, kind).is_some()
}

/// Обход прямых потомков без промежуточного `Vec`.
pub fn for_each_child<'tree, E>(
    node: TsNode<'tree>,
    mut visit: impl FnMut(TsNode<'tree>) -> Result<(), E>,
) -> Result<(), E> {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return Ok(());
    }
    loop {
        visit(cursor.node())?;
        if !cursor.goto_next_sibling() {
            return Ok(());
        }
    }
}

/// Позиции tree-sitter (0-based) → Span (1-based).
pub fn span(node: TsNode<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span {
        start_line: start.row as u32 + 1,
        start_col: start.column as u32 + 1,
        end_line: end.row as u32 + 1,
        end_col: end.column as u32 + 1,
    }
}

/// Первый `identifier` в прямом порядке обхода.
///
/// Так берётся ведущее имя составного типа: `list[int]`, `Optional[str]`.
pub fn first_identifier<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    children(node).into_iter().find_map(first_identifier)
}

/// Самые внешние `call`-узлы: внутрь найденного вызова не спускаемся,
/// вложенные вызовы в аргументах разберёт сканер значений.
pub fn find_calls<'tree>(node: TsNode<'tree>, out: &mut Vec<TsNode<'tree>>) {
    if node.kind() == "call" {
        out.push(node);
        return;
    }
    for child in children(node) {
        find_calls(child, out);
    }
}

/// Именованные потомки — без пунктуации и прочих анонимных узлов.
pub fn named_children<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Прогоняет tree-sitter запрос по поддереву.
///
/// Возвращает по одной карте захватов на совпадение: `@имя` → узлы.
/// Так шаблоны описываются декларативно (язык запросов `.scm`), а не
/// ветками ручного визитора.
pub fn run_query<'q, 'tree>(
    query: &'q tree_sitter::Query,
    root: TsNode<'tree>,
    source: &[u8],
) -> Vec<HashMap<&'q str, Vec<TsNode<'tree>>>> {
    use streaming_iterator::StreamingIterator;

    // Имена захватов живут в самом запросе, поэтому наружу отдаются
    // ссылками — на каждое совпадение иначе набегала бы аллокация строки.
    let names = query.capture_names();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, root, source);

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        let mut captures: HashMap<&'q str, Vec<TsNode<'tree>>> = HashMap::new();
        for capture in m.captures {
            captures
                .entry(names[capture.index as usize])
                .or_default()
                .push(capture.node);
        }
        out.push(captures);
    }
    out
}
