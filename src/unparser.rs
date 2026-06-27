use crate::graph::{AstGraph, NodeId};
use std::collections::HashMap;

/// Serialize an AST graph back to source text.
pub fn unparse(graph: &AstGraph) -> String {
    let mut out = String::new();
    unparse_node(graph, graph.root, None, &mut out, &mut HashMap::new());
    out.push_str(&graph.root_trailing_trivia);
    out
}

fn unparse_node(
    graph: &AstGraph,
    node_id: NodeId,
    parent: Option<NodeId>,
    out: &mut String,
    seen_counts: &mut HashMap<NodeId, u32>,
) {
    let node = graph.get(&node_id).expect("node must exist");
    if let Some(parent_id) = parent {
        let occ = *seen_counts.entry(node_id).or_insert(0);
        *seen_counts.entry(node_id).or_insert(0) = occ + 1;
        out.push_str(graph.get_trivia(parent_id, node_id, occ));
    }

    if node.is_leaf() {
        out.push_str(&node.payload);
        return;
    }

    let mut child_counts: HashMap<NodeId, u32> = HashMap::new();
    for child_id in &node.children {
        unparse_node(graph, *child_id, Some(node_id), out, &mut child_counts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::parse_rust;

    #[test]
    fn roundtrip_simple_function() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let graph = parse_rust(src).unwrap();
        let out = unparse(&graph);
        assert_eq!(src.as_bytes(), out.as_bytes());
    }

    #[test]
    fn roundtrip_with_macro() {
        let src = "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{}\", x + y);\n}\n";
        let graph = parse_rust(src).unwrap();
        let out = unparse(&graph);
        assert_eq!(src.as_bytes(), out.as_bytes());
    }

    #[test]
    fn roundtrip_with_block_edit() {
        let src = "fn foo() {\n    let x = 1;\n}\n";
        let graph = parse_rust(src).unwrap();
        let out = unparse(&graph);
        assert_eq!(src.as_bytes(), out.as_bytes());
    }

    #[test]
    fn roundtrip_go_multiline_return() {
        use crate::SourceLanguage;

        let src = "package main\n\nfunc greet(name string) string {\n    return fmt.Sprintf(\"Hi, %s!\", name)\n}\n";
        let graph = crate::frontend::parse_language(SourceLanguage::Go, src).unwrap();
        let out = unparse(&graph);
        assert_eq!(src.as_bytes(), out.as_bytes());
    }
}
