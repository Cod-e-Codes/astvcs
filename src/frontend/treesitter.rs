use crate::frontend::languages::SourceLanguage;
use crate::graph::{AstGraph, Node, NodeId, NodeKind};
use std::collections::HashMap;
use tree_sitter::{Node as TsNode, Parser, Tree};

/// Parse Rust source into an internal AST graph.
pub fn parse_rust(source: &str) -> Result<AstGraph, String> {
    parse_language(SourceLanguage::Rust, source)
}

/// Parse source using the language inferred from `path`.
pub fn parse_source(path: &str, source: &str) -> Result<AstGraph, String> {
    let lang = SourceLanguage::from_path(path)
        .ok_or_else(|| format!("no AST frontend for extension: {path}"))?;
    parse_language(lang, source)
}

pub fn parse_language(lang: SourceLanguage, source: &str) -> Result<AstGraph, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.tree_sitter_language())
        .map_err(|e| format!("failed to set {} grammar: {e}", lang.as_str()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "parse returned no tree".to_string())?;
    if tree.root_node().has_error() {
        return Err("syntax errors in source".into());
    }
    parse_tree(source, &tree)
}

fn parse_tree(source: &str, tree: &Tree) -> Result<AstGraph, String> {
    let mut nodes = HashMap::new();
    let mut pending_trivia: Vec<(NodeId, NodeId, u32, String)> = Vec::new();
    let root_id = translate(source, tree.root_node(), &mut nodes, &mut pending_trivia)?;
    let root = nodes.get(&root_id).cloned().unwrap();
    let mut graph = AstGraph::new(root, nodes);

    for (parent, child, occurrence, leading) in pending_trivia {
        graph.set_trivia(parent, child, occurrence, leading);
    }

    let root_ts = tree.root_node();
    if let Some(last_named) = last_child(root_ts) {
        let tail_start = last_named.end_byte();
        if tail_start < source.len() {
            graph.root_trailing_trivia = source[tail_start..].to_string();
        }
    }

    Ok(graph)
}

fn last_child(node: TsNode) -> Option<TsNode> {
    let mut cursor = node.walk();
    let mut last = None;
    if cursor.goto_first_child() {
        loop {
            last = Some(cursor.node());
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    last
}

/// End byte of the rightmost leaf under `node`.
///
/// Named tree-sitter nodes often extend their span through trailing whitespace
/// that belongs before a following sibling (for example Go `statement_list`
/// through the newline before `}`). Leading trivia must start after that leaf,
/// not after the named node's extended end byte.
fn last_leaf_end_byte(mut node: TsNode) -> usize {
    loop {
        let mut cursor = node.walk();
        if cursor.goto_last_child() {
            node = cursor.node();
        } else {
            return node.end_byte();
        }
    }
}

fn composite_leaf_span(ts_node: TsNode, source: &str) -> Option<String> {
    if ts_node.child_count() == 0 || !ts_node.is_named() {
        return None;
    }
    if !matches!(
        ts_node.kind(),
        "integer_value"
            | "float_value"
            | "plain_value"
            | "color_value"
            | "universal_value"
            | "string_value"
    ) {
        return None;
    }
    let span = source[ts_node.start_byte()..ts_node.end_byte()].to_string();
    if span.is_empty() || span.len() > 64 {
        return None;
    }
    if span
        .chars()
        .any(|c| c.is_whitespace() || c == '{' || c == '}' || c == '<' || c == '>')
    {
        return None;
    }
    Some(span)
}

fn translate(
    source: &str,
    ts_node: TsNode,
    nodes: &mut HashMap<NodeId, Node>,
    pending_trivia: &mut Vec<(NodeId, NodeId, u32, String)>,
) -> Result<NodeId, String> {
    let kind = if ts_node.is_named() {
        NodeKind::from_ts_kind(ts_node.kind())
    } else {
        NodeKind::Token
    };

    if let Some(span) = composite_leaf_span(ts_node, source) {
        let node = Node::leaf(kind, span);
        let node_id = node.id;
        nodes.insert(node_id, node);
        return Ok(node_id);
    }

    let payload = if ts_node.child_count() == 0 {
        source[ts_node.start_byte()..ts_node.end_byte()].to_string()
    } else {
        String::new()
    };

    let mut child_ids = Vec::new();
    let mut child_leading = Vec::new();

    let mut cursor = ts_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child_ts = cursor.node();
            let leading_start = if let Some(prev) = child_ts.prev_sibling() {
                last_leaf_end_byte(prev)
            } else {
                ts_node.start_byte()
            };
            let leading = if leading_start < child_ts.start_byte() {
                source[leading_start..child_ts.start_byte()].to_string()
            } else {
                String::new()
            };

            let child_id = translate(source, child_ts, nodes, pending_trivia)?;
            child_ids.push(child_id);
            child_leading.push(leading);

            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    if kind == NodeKind::Comment && !child_ids.is_empty() && ts_node.next_sibling().is_none() {
        let mut cursor = ts_node.walk();
        if cursor.goto_last_child() {
            let last_child_ts = cursor.node();
            let tail_start = last_leaf_end_byte(last_child_ts);
            if tail_start < ts_node.end_byte() {
                let tail = source[tail_start..ts_node.end_byte()].to_string();
                if !tail.is_empty() {
                    let old_last_id = *child_ids.last().unwrap();
                    if let Some(old) = nodes.get(&old_last_id).cloned()
                        && old.is_leaf()
                    {
                        let new_payload = format!("{}{}", old.payload, tail);
                        let new_node = Node::new(old.kind.clone(), new_payload, vec![]);
                        let new_id = new_node.id;
                        nodes.remove(&old_last_id);
                        nodes.insert(new_id, new_node);
                        *child_ids.last_mut().unwrap() = new_id;
                    }
                }
            }
        }
    }

    let node = Node::new(kind, payload, child_ids.clone());
    let node_id = node.id;
    nodes.insert(node_id, node);

    let mut occurrence_counts: HashMap<NodeId, u32> = HashMap::new();
    for (child_id, leading) in child_ids.iter().zip(child_leading.iter()) {
        let occ = *occurrence_counts.entry(*child_id).or_insert(0);
        occurrence_counts.insert(*child_id, occ + 1);
        pending_trivia.push((node_id, *child_id, occ, leading.clone()));
    }

    Ok(node_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::languages::SourceLanguage;

    #[test]
    fn eof_line_comment_roundtrip() {
        use crate::unparser::unparse;
        let src = "fn main() {}\n// left comment\n";
        let graph = parse_rust(src).unwrap();
        assert_eq!(unparse(&graph), src);
    }

    #[test]
    fn parses_simple_function() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let graph = parse_rust(src).unwrap();
        graph.validate().unwrap();
        assert!(graph.nodes.len() > 1);
    }

    #[test]
    fn parses_python() {
        let src = "def add(a, b):\n    return a + b\n";
        let graph = parse_language(SourceLanguage::Python, src).unwrap();
        graph.validate().unwrap();
    }

    #[test]
    fn parses_javascript() {
        let src = "function add(a, b) { return a + b; }\n";
        let graph = parse_language(SourceLanguage::JavaScript, src).unwrap();
        graph.validate().unwrap();
    }

    #[test]
    fn parses_go_mod() {
        let src = "module example.com/demo\n\ngo 1.22\n";
        let graph = parse_source("go.mod", src).unwrap();
        graph.validate().unwrap();
    }

    #[test]
    fn parses_go() {
        let src = "package main\nfunc main() {}\n";
        let graph = parse_language(SourceLanguage::Go, src).unwrap();
        graph.validate().unwrap();
    }

    #[test]
    fn parses_json() {
        let src = r#"{"key": "value"}"#;
        let graph = parse_language(SourceLanguage::Json, src).unwrap();
        graph.validate().unwrap();
    }

    #[test]
    fn typescript_and_tsx_use_distinct_grammars() {
        use tree_sitter::{Node, Parser};

        fn has_named_kind(node: Node, kind: &str) -> bool {
            if node.is_named() && node.kind() == kind {
                return true;
            }
            let mut cursor = node.walk();
            if cursor.goto_first_child() {
                loop {
                    if has_named_kind(cursor.node(), kind) {
                        return true;
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            false
        }

        let plain = "function main(): void {}\n";
        parse_source("app.ts", plain).unwrap().validate().unwrap();
        parse_source("view.tsx", plain).unwrap().validate().unwrap();

        let jsx = "export function View() { return <div />; }\n";
        assert!(parse_source("app.ts", jsx).is_err());
        let tsx_graph = parse_source("view.tsx", jsx).unwrap();
        tsx_graph.validate().unwrap();

        let mut ts_parser = Parser::new();
        ts_parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("ts grammar");
        let ts_tree = ts_parser.parse(jsx, None).expect("ts tree");
        assert!(ts_tree.root_node().has_error());
        assert!(!has_named_kind(
            ts_tree.root_node(),
            "jsx_self_closing_element"
        ));

        let mut tsx_parser = Parser::new();
        tsx_parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .expect("tsx grammar");
        let tsx_tree = tsx_parser.parse(jsx, None).expect("tsx tree");
        assert!(!tsx_tree.root_node().has_error());
        assert!(has_named_kind(
            tsx_tree.root_node(),
            "jsx_self_closing_element"
        ));
    }

    #[test]
    fn parses_html() {
        let src = "<!DOCTYPE html><html><body><p>hi</p></body></html>\n";
        let graph = parse_language(SourceLanguage::Html, src).unwrap();
        graph.validate().unwrap();
    }

    #[test]
    fn css_dimension_literals_roundtrip() {
        use crate::diff::diff_graphs;
        use crate::graph::Mutation;
        use crate::unparser::unparse;

        let base = ".box {\n    margin-top: 10px;\n    padding-bottom: 20px;\n}\n";
        let left = ".box {\n    margin-top: 11px;\n    padding-bottom: 20px;\n}\n";
        let right = ".box {\n    margin-top: 10px;\n    padding-bottom: 19px;\n}\n";
        let b = parse_source("calc.css", base).unwrap();
        let l = parse_source("calc.css", left).unwrap();
        let r = parse_source("calc.css", right).unwrap();
        assert_eq!(unparse(&b), base);
        assert_eq!(unparse(&l), left);
        assert_eq!(unparse(&r), right);
        let left_diff = diff_graphs(&b, &l);
        assert!(
            left_diff.mutations.iter().any(
                |m| matches!(m, Mutation::EditPayload { new_payload, .. } if new_payload == "11px")
            ),
            "expected margin edit: {:?}",
            left_diff.mutations
        );
        let merged = crate::merge::merge_files(
            &crate::frontend::FileContent::Ast(b),
            &crate::frontend::FileContent::Ast(l),
            &crate::frontend::FileContent::Ast(r),
        );
        let crate::merge::MergeOutcome::Merged(crate::frontend::FileContent::Ast(g)) = merged
        else {
            panic!("expected merge, got {merged:?}");
        };
        let text = unparse(&g);
        assert!(text.contains("11px"), "{text}");
        assert!(text.contains("19px"), "{text}");
        assert!(!text.contains("10px"), "{text}");
        assert!(!text.contains("20px"), "{text}");
    }

    #[test]
    fn parses_css() {
        let src = "body { color: red; }\n";
        let graph = parse_language(SourceLanguage::Css, src).unwrap();
        graph.validate().unwrap();
    }
}
