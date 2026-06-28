use tree_sitter::Language;

/// Supported source languages mapped from file extensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceLanguage {
    Rust,
    Python,
    JavaScript,
    C,
    Go,
    Json,
    Toml,
    Yaml,
    TypeScript,
    Tsx,
    Cpp,
    Java,
    CSharp,
    Swift,
    Kotlin,
    Zig,
    Sql,
    Bash,
    GoMod,
}

impl SourceLanguage {
    pub fn from_path(path: &str) -> Option<Self> {
        let basename = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        if basename == "go.mod" {
            return Some(Self::GoMod);
        }
        let ext = path.rsplit('.').next()?;
        match ext {
            "rs" => Some(Self::Rust),
            "py" | "pyw" => Some(Self::Python),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "c" | "h" => Some(Self::C),
            "go" => Some(Self::Go),
            "json" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => Some(Self::Cpp),
            "java" => Some(Self::Java),
            "cs" => Some(Self::CSharp),
            "swift" => Some(Self::Swift),
            "kt" | "kts" => Some(Self::Kotlin),
            "zig" => Some(Self::Zig),
            "sql" => Some(Self::Sql),
            "sh" | "bash" => Some(Self::Bash),
            _ => None,
        }
    }

    pub fn tree_sitter_language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Self::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Swift => tree_sitter_swift::LANGUAGE.into(),
            Self::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Self::Zig => tree_sitter_zig::LANGUAGE.into(),
            Self::Sql => tree_sitter_sequel::LANGUAGE.into(),
            Self::Bash => tree_sitter_bash::LANGUAGE.into(),
            Self::GoMod => tree_sitter_gomod_orchard::LANGUAGE.into(),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::C => "c",
            Self::Go => "go",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Cpp => "cpp",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::Swift => "swift",
            Self::Kotlin => "kotlin",
            Self::Zig => "zig",
            Self::Sql => "sql",
            Self::Bash => "bash",
            Self::GoMod => "gomod",
        }
    }
}

pub fn supported_extensions() -> &'static [&'static str] {
    &[
        "rs", "py", "pyw", "js", "mjs", "cjs", "c", "h", "go", "json", "toml", "yaml", "yml", "ts",
        "tsx", "cpp", "cc", "cxx", "hpp", "hh", "java", "cs", "swift", "kt", "kts", "zig", "sql",
        "sh", "bash",
    ]
}

/// Basename paths parsed with a dedicated frontend (not extension-based).
pub fn supported_special_paths() -> &'static [&'static str] {
    &["go.mod"]
}

/// Paths that are intentionally stored as text blobs without a user-facing warning.
pub fn is_text_only_path(path: &str) -> bool {
    if SourceLanguage::from_path(path).is_some() {
        return false;
    }
    let basename = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    if matches!(
        basename,
        ".gitignore"
            | ".dockerignore"
            | ".astvcsignore"
            | "Dockerfile"
            | "Makefile"
            | "LICENSE"
            | "COPYING"
            | "README"
            | "go.sum"
    ) || basename.ends_with("ignore") && basename.starts_with('.')
    {
        return true;
    }
    let ext = path.rsplit('.').next().unwrap_or("");
    matches!(
        ext,
        "md" | "txt"
            | "rst"
            | "log"
            | "csv"
            | "ini"
            | "cfg"
            | "env"
            | "gitignore"
            | "dockerignore"
            | "editorconfig"
            | "ps1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions_match_from_path() {
        for ext in supported_extensions() {
            let path = format!("file.{ext}");
            assert!(
                SourceLanguage::from_path(&path).is_some(),
                "from_path missing extension: {ext}"
            );
        }
    }

    #[test]
    fn maps_extensions() {
        assert_eq!(
            SourceLanguage::from_path("main.rs"),
            Some(SourceLanguage::Rust)
        );
        assert_eq!(
            SourceLanguage::from_path("app.py"),
            Some(SourceLanguage::Python)
        );
        assert_eq!(
            SourceLanguage::from_path("index.js"),
            Some(SourceLanguage::JavaScript)
        );
        assert_eq!(SourceLanguage::from_path("main.c"), Some(SourceLanguage::C));
        assert_eq!(
            SourceLanguage::from_path("main.go"),
            Some(SourceLanguage::Go)
        );
        assert_eq!(
            SourceLanguage::from_path("data.json"),
            Some(SourceLanguage::Json)
        );
        assert_eq!(
            SourceLanguage::from_path("Cargo.toml"),
            Some(SourceLanguage::Toml)
        );
        assert_eq!(
            SourceLanguage::from_path("config.yaml"),
            Some(SourceLanguage::Yaml)
        );
        assert_eq!(
            SourceLanguage::from_path("app.ts"),
            Some(SourceLanguage::TypeScript)
        );
        assert_eq!(
            SourceLanguage::from_path("view.tsx"),
            Some(SourceLanguage::Tsx)
        );
        assert_eq!(
            SourceLanguage::from_path("main.cpp"),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            SourceLanguage::from_path("Main.java"),
            Some(SourceLanguage::Java)
        );
        assert_eq!(
            SourceLanguage::from_path("Program.cs"),
            Some(SourceLanguage::CSharp)
        );
        assert_eq!(
            SourceLanguage::from_path("main.swift"),
            Some(SourceLanguage::Swift)
        );
        assert_eq!(
            SourceLanguage::from_path("main.kt"),
            Some(SourceLanguage::Kotlin)
        );
        assert_eq!(
            SourceLanguage::from_path("main.zig"),
            Some(SourceLanguage::Zig)
        );
        assert_eq!(
            SourceLanguage::from_path("query.sql"),
            Some(SourceLanguage::Sql)
        );
        assert_eq!(
            SourceLanguage::from_path("script.sh"),
            Some(SourceLanguage::Bash)
        );
        assert_eq!(
            SourceLanguage::from_path("go.mod"),
            Some(SourceLanguage::GoMod)
        );
        assert_eq!(
            SourceLanguage::from_path("subdir/go.mod"),
            Some(SourceLanguage::GoMod)
        );
        assert_eq!(
            SourceLanguage::from_path("header.h"),
            Some(SourceLanguage::C)
        );
        assert_eq!(
            SourceLanguage::from_path("widget.hpp"),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            SourceLanguage::from_path("types.d.ts"),
            Some(SourceLanguage::TypeScript)
        );
        assert_eq!(SourceLanguage::from_path("README.RS"), None);
        assert_eq!(SourceLanguage::from_path("noext"), None);
        assert_eq!(SourceLanguage::from_path("readme.md"), None);
        assert!(is_text_only_path("go.sum"));
        assert!(is_text_only_path("scripts/run.ps1"));
    }
}
