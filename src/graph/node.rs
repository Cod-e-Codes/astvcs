use sha2::{Digest, Sha256};
use std::fmt;

/// Content-addressed identifier for an AST node.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct NodeId([u8; 32]);

impl NodeId {
    pub fn nil() -> Self {
        Self([0u8; 32])
    }

    pub fn from_parts(kind: &str, payload: &str, children: &[NodeId]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update([0]);
        hasher.update(payload.as_bytes());
        hasher.update([0]);
        for child in children {
            hasher.update(child.0);
        }
        Self(hasher.finalize().into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn short_hex(&self) -> String {
        hex::encode(&self.0[..6])
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.short_hex())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

/// Semantic category of an AST node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum NodeKind {
    Module,
    Function,
    Struct,
    Enum,
    Impl,
    Block,
    Loop,
    Conditional,
    Declaration,
    Parameter,
    Field,
    Expression,
    Statement,
    Type,
    Identifier,
    Literal,
    Token,
    Comment,
    Unknown(String),
}

impl NodeKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Module => "Module",
            Self::Function => "Function",
            Self::Struct => "Struct",
            Self::Enum => "Enum",
            Self::Impl => "Impl",
            Self::Block => "Block",
            Self::Loop => "Loop",
            Self::Conditional => "Conditional",
            Self::Declaration => "Declaration",
            Self::Parameter => "Parameter",
            Self::Field => "Field",
            Self::Expression => "Expression",
            Self::Statement => "Statement",
            Self::Type => "Type",
            Self::Identifier => "Identifier",
            Self::Literal => "Literal",
            Self::Token => "Token",
            Self::Comment => "Comment",
            Self::Unknown(s) => s,
        }
    }

    pub fn from_ts_kind(kind: &str) -> Self {
        match kind {
            "source_file" | "module" | "program" => Self::Module,
            "function_item"
            | "function_signature"
            | "function_definition"
            | "function_declaration" => Self::Function,
            "struct_item" | "class_definition" | "class_declaration" => Self::Struct,
            "enum_item" | "enum_declaration" => Self::Enum,
            "impl_item" => Self::Impl,
            "block" | "compound_statement" | "statement_block" => Self::Block,
            "for_expression" | "while_expression" | "loop_expression" | "for_statement"
            | "while_statement" => Self::Loop,
            "if_expression" | "match_expression" | "if_statement" | "switch_statement" => {
                Self::Conditional
            }
            "let_declaration"
            | "const_item"
            | "static_item"
            | "assignment"
            | "variable_declaration"
            | "lexical_declaration" => Self::Declaration,
            "parameter" => Self::Parameter,
            "field_declaration" | "field_expression" => Self::Field,
            "call_expression"
            | "binary_expression"
            | "unary_expression"
            | "assignment_expression"
            | "macro_invocation" => Self::Expression,
            "expression_statement" => Self::Statement,
            "type_identifier" | "primitive_type" | "reference_type" => Self::Type,
            "identifier" => Self::Identifier,
            "integer_literal" | "float_literal" | "string_literal" | "char_literal" | "number"
            | "string" | "true" | "false" | "null" => Self::Literal,
            "line_comment" | "block_comment" | "comment" => Self::Comment,
            "" => Self::Token,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// A node in the semantic AST graph.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub payload: String,
    pub children: Vec<NodeId>,
}

impl Node {
    pub fn new(kind: NodeKind, payload: String, children: Vec<NodeId>) -> Self {
        let id = NodeId::from_parts(kind.as_str(), &payload, &children);
        Self {
            id,
            kind,
            payload,
            children,
        }
    }

    pub fn leaf(kind: NodeKind, payload: String) -> Self {
        Self::new(kind, payload, vec![])
    }

    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}
