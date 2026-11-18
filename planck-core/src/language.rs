use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt::Write,
    path::Path,
    str::FromStr,
};

use once_cell::sync::Lazy;
use planck_rpc::style::{LineStyle, Style};
use regex::Regex;
use strum_macros::{AsRefStr, Display, EnumMessage, EnumString, IntoStaticStr};
use tracing::{Level, event};
use tree_sitter::{Point, TreeCursor};

use crate::{
    directory::Directory,
    syntax::highlight::{HighlightConfiguration, HighlightIssue},
};

#[remain::sorted]
pub enum Indent {
    Space(u8),
    Tab,
}

impl Indent {
    const fn tab() -> &'static str {
        Indent::Tab.as_str()
    }

    const fn space(count: u8) -> &'static str {
        Indent::Space(count).as_str()
    }

    const fn as_str(&self) -> &'static str {
        match self {
            Indent::Tab => "\u{0009}",
            #[allow(clippy::wildcard_in_or_patterns)]
            Indent::Space(v) => match v {
                2 => "\u{0020}\u{0020}",
                4 => "\u{0020}\u{0020}\u{0020}\u{0020}",
                8 | _ => {
                    "\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}\u{0020}"
                }
            },
        }
    }
}

const DEFAULT_CODE_GLANCE_LIST: &[&str] = &["source_file"];
const DEFAULT_CODE_GLANCE_IGNORE_LIST: &[&str] = &["source_file"];

#[macro_export]
macro_rules! comment_properties {
    () => {
        CommentProperties {
            single_line_start: None,
            single_line_end: None,

            multi_line_start: None,
            multi_line_end: None,
            multi_line_prefix: None,
        }
    };
    ($s:expr) => {
        CommentProperties {
            single_line_start: Some($s),
            single_line_end: None,

            multi_line_start: None,
            multi_line_end: None,
            multi_line_prefix: None,
        }
    };
    ($s:expr, $e:expr) => {
        CommentProperties {
            single_line_start: Some($s),
            single_line_end: Some($e),

            multi_line_start: None,
            multi_line_end: None,
            multi_line_prefix: None,
        }
    };
    ($sl_s:expr, $sl_e:expr, $ml_s:expr, $ml_e:expr) => {
        CommentProperties {
            single_line_start: Some($sl_s),
            single_line_end: Some($sl_e),

            multi_line_start: Some($sl_s),
            multi_line_end: None,
            multi_line_prefix: Some($sl_e),
        }
    };
}

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, PartialOrd, Ord, Default)]
pub struct SyntaxProperties {
    /// An extra check to make sure that the array elements are in the correct order.  
    /// If this id does not match the enum value, a panic will happen with a debug assertion message.
    id: PlanckLanguage,

    /// All tokens that can be used for comments in language
    comment: CommentProperties,
    /// The indent unit.  
    /// "  " for bash, "    " for rust, for example.
    indent: &'static str,
    /// Filenames that belong to this language  
    /// `["Dockerfile"]` for Dockerfile, `[".editorconfig"]` for EditorConfig
    files: &'static [&'static str],
    /// File name extensions to determine the language.  
    /// `["py"]` for python, `["rs"]` for rust, for example.
    extensions: &'static [&'static str],
    /// Tree-sitter properties
    tree_sitter: TreeSitterProperties,
}

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, PartialOrd, Ord, Default)]
struct TreeSitterProperties {
    /// the grammar name that's in the grammars folder
    grammar: Option<&'static str>,
    /// the grammar fn name
    grammar_fn: Option<&'static str>,
    /// the query folder name
    query: Option<&'static str>,
    /// Preface: Originally this feature was called "Code Lens", which is not
    /// an LSP "Code Lens". It is renamed to "Code Glance", below doc text is
    /// left unchanged.  
    ///
    /// Lists of tree-sitter node types that control how code lenses are built.
    /// The first is a list of nodes that should be traversed and included in
    /// the lens, along with their children. The second is a list of nodes that
    /// should be excluded from the lens, though they will still be traversed.
    /// See `walk_tree` for more details.
    ///
    /// The tree-sitter playground may be useful when creating these lists:
    /// https://tree-sitter.github.io/tree-sitter/playground
    ///
    /// If unsure, use `DEFAULT_CODE_GLANCE_LIST` and
    /// `DEFAULT_CODE_GLANCE_IGNORE_LIST`.
    code_glance: (&'static [&'static str], &'static [&'static str]),
    /// the tree-sitter tag names that can be put in sticky headers
    sticky_headers: &'static [&'static str],
}

impl TreeSitterProperties {
    const DEFAULT: Self = Self {
        grammar: None,
        grammar_fn: None,
        query: None,
        code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
        sticky_headers: &[],
    };

    /// Grammar file, grammar symbol and query folder all called `name`.
    const fn named(name: &'static str) -> Self {
        Self {
            grammar: Some(name),
            grammar_fn: Some(name),
            query: Some(name),
            ..Self::DEFAULT
        }
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, PartialOrd, Ord, Default)]
struct CommentProperties {
    /// Single line comment token used when commenting out one line.
    /// "#" for python, "//" for rust for example.
    single_line_start: Option<&'static str>,
    single_line_end: Option<&'static str>,

    /// Multi line comment token used when commenting a selection of lines.
    /// "#" for python, "//" for rust for example.
    multi_line_start: Option<&'static str>,
    multi_line_end: Option<&'static str>,
    multi_line_prefix: Option<&'static str>,
}

/// NOTE: Keep the enum variants "fieldless" so they can cast to usize as array
/// indices into the LANGUAGES array.  See method `PlanckLanguage::properties`.
///
/// Do not assign values to the variants because the number of variants and
/// number of elements in the LANGUAGES array change as different features
/// selected by the cargo build command.
#[derive(
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Clone,
    Copy,
    Debug,
    Display,
    AsRefStr,
    IntoStaticStr,
    EnumString,
    EnumMessage,
    Default,
)]
#[strum(ascii_case_insensitive)]
#[remain::sorted]
pub enum PlanckLanguage {
    // Do not move
    #[remain::unsorted]
    #[default]
    #[strum(message = "Plain Text")]
    PlainText,

    #[strum(message = "Ada")]
    Ada,
    #[strum(message = "Adl")]
    Adl,
    #[strum(message = "Agda")]
    Agda,
    #[strum(message = "Astro")]
    Astro,
    #[strum(message = "Bash")]
    Bash,
    #[strum(message = "Bass")]
    Bass,
    #[strum(message = "Beancount")]
    Beancount,
    #[strum(message = "Bibtex")]
    Bibtex,
    #[strum(message = "Bitbake")]
    Bitbake,
    #[strum(message = "Blade")]
    Blade,
    #[strum(message = "C")]
    C,
    #[strum(message = "Clojure")]
    Clojure,
    #[strum(message = "CMake")]
    Cmake,
    #[strum(message = "Comment")]
    Comment,
    #[strum(message = "C++")]
    Cpp,
    #[strum(message = "C#")]
    Csharp,
    #[strum(message = "CSS")]
    Css,
    #[strum(message = "Cue")]
    Cue,
    #[strum(message = "D")]
    D,
    #[strum(message = "Dart")]
    Dart,
    #[strum(message = "Dhall")]
    Dhall,
    #[strum(message = "Diff")]
    Diff,
    #[strum(message = "Dockerfile")]
    Dockerfile,
    #[strum(message = "Dot")]
    Dot,
    #[strum(message = "Elixir")]
    Elixir,
    #[strum(message = "Elm")]
    Elm,
    #[strum(message = "Erlang")]
    Erlang,
    #[strum(message = "Fish Shell")]
    Fish,
    #[strum(message = "Fluent")]
    Fluent,
    #[strum(message = "Forth")]
    Forth,
    #[strum(message = "Fortran")]
    Fortran,
    #[strum(message = "F#")]
    FSharp,
    #[strum(message = "Gitattributes")]
    Gitattributes,
    #[strum(message = "Git (commit)")]
    GitCommit,
    #[strum(message = "Git (config)")]
    GitConfig,
    #[strum(message = "Git (ignore)")]
    Gitignore,
    #[strum(message = "Git (rebase)")]
    GitRebase,
    #[strum(message = "Gleam")]
    Gleam,
    #[strum(message = "Glimmer")]
    Glimmer,
    #[strum(message = "GLSL")]
    Glsl,
    #[strum(message = "Gn")]
    Gn,
    #[strum(message = "Go")]
    Go,
    #[strum(message = "Go (go.mod)")]
    GoMod,
    #[strum(message = "Go (template)")]
    GoTemplate,
    #[strum(message = "Go (go.work)")]
    GoWork,
    #[strum(message = "GraphQL")]
    GraphQl,
    #[strum(message = "Groovy")]
    Groovy,
    #[strum(message = "Hare")]
    Hare,
    #[strum(message = "Haskell")]
    Haskell,
    #[strum(message = "Haxe")]
    Haxe,
    #[strum(message = "HCL")]
    Hcl,
    #[strum(message = "Hex Dump")]
    HexDump,
    #[strum(message = "Hosts file (/etc/hosts)")]
    Hosts,
    #[strum(message = "HTML")]
    Html,
    #[strum(message = "INI")]
    Ini,
    #[strum(message = "Java")]
    Java,
    #[strum(message = "JavaScript")]
    Javascript,
    #[strum(message = "Jinja2")]
    Jinja2,
    #[strum(message = "JSDoc")]
    Jsdoc,
    #[strum(message = "JSON")]
    Json,
    #[strum(message = "JSON5")]
    Json5,
    #[strum(message = "Jsonnet")]
    Jsonnet,
    #[strum(message = "JavaScript React")]
    Jsx,
    #[strum(message = "Julia")]
    Julia,
    #[strum(message = "Just")]
    Just,
    #[strum(message = "KDL")]
    Kdl,
    #[strum(message = "Kotlin")]
    Kotlin,
    #[strum(message = "Kotlin Build Script")]
    KotlinBuildScript,
    #[strum(message = "LaTeX")]
    Latex,
    #[strum(message = "Linker Script")]
    Ld,
    #[strum(message = "LLVM")]
    Llvm,
    #[strum(message = "LLVM MIR")]
    LlvmMir,
    #[strum(message = "Log")]
    Log,
    #[strum(message = "Lua")]
    Lua,
    #[strum(message = "Makefile")]
    Make,
    #[strum(message = "Markdown")]
    Markdown,
    #[strum(serialize = "markdown.inline")]
    MarkdownInline,
    #[strum(message = "Meson")]
    Meson,
    #[strum(message = "NASM")]
    Nasm,
    #[strum(message = "Nix")]
    Nix,
    #[strum(message = "Nu (nushell)")]
    Nushell,
    #[strum(message = "Ocaml")]
    Ocaml,
    #[strum(serialize = "ocaml.interface")]
    OcamlInterface,
    #[strum(message = "Odin")]
    Odin,
    #[strum(message = "OpenCL")]
    OpenCl,
    #[strum(message = "Pascal")]
    Pascal,
    #[strum(message = "Password file (/etc/passwd)")]
    Passwd,
    #[strum(message = "PEM (RFC 1422)")]
    Pem,
    #[strum(message = "PHP")]
    Php,
    #[strum(message = "PKL")]
    Pkl,
    #[strum(message = "PowerShell")]
    PowerShell,
    #[strum(message = "Prisma")]
    Prisma,
    #[strum(message = "Proto")]
    ProtoBuf,
    #[strum(message = "Python")]
    Python,
    #[strum(message = "QL")]
    Ql,
    #[strum(message = "R")]
    R,
    #[strum(message = "RCL")]
    Rcl,
    #[strum(message = "RegEx")]
    Regex,
    #[strum(message = "REGO")]
    Rego,
    #[strum(message = "RON (Rust Object Notation)")]
    Ron,
    #[strum(message = "Rst")]
    Rst,
    #[strum(message = "Ruby")]
    Ruby,
    #[strum(message = "Rust")]
    Rust,
    #[strum(message = "Scala")]
    Scala,
    #[strum(message = "Scheme")]
    Scheme,
    #[strum(message = "SCSS")]
    Scss,
    #[strum(message = "Shell Script (POSIX)")]
    ShellScript,
    #[strum(message = "Smithy")]
    Smithy,
    #[strum(message = "SQL")]
    Sql,
    #[strum(message = "SSH Config")]
    SshClientConfig,
    #[strum(message = "Strace")]
    Strace,
    #[strum(message = "Svelte")]
    Svelte,
    #[strum(message = "Sway")]
    Sway,
    #[strum(message = "Swift")]
    Swift,
    #[strum(message = "TCL")]
    Tcl,
    #[strum(message = "TOML")]
    Toml,
    #[strum(message = "Tsx")]
    Tsx,
    #[strum(message = "TypeScript")]
    Typescript,
    #[strum(message = "Typst")]
    Typst,
    #[strum(message = "Verilog")]
    Verilog,
    #[strum(message = "Vue")]
    Vue,
    #[strum(message = "WASM")]
    Wasm,
    #[strum(message = "WGSL")]
    Wgsl,
    #[strum(message = "WIT")]
    Wit,
    #[strum(message = "XML")]
    Xml,
    #[strum(message = "YAML")]
    Yaml,
    #[strum(message = "Zig")]
    Zig,
}

/// NOTE: Elements in the array must be in the same order as the enum variants of
/// `PlanckLanguage` as they will be accessed using the enum variants as indices.
const LANGUAGES: &[SyntaxProperties] = &[
    // Undetected/unmatched fallback or just plain file
    SyntaxProperties {
        id: PlanckLanguage::PlainText,
        indent: Indent::tab(),
        files: &[],
        extensions: &["txt"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    // Languages
    SyntaxProperties {
        id: PlanckLanguage::Ada,
        indent: Indent::tab(),
        files: &[],
        extensions: &["adb", "ads"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Adl,
        indent: Indent::tab(),
        files: &[],
        extensions: &["adl"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Agda,
        indent: Indent::tab(),
        files: &[],
        extensions: &["agda"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Astro,
        indent: Indent::tab(),
        files: &[],
        extensions: &["astro"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Bash,
        indent: Indent::space(2),
        files: &[
            ".bashrc",
            ".bash_profile",
            ".bash_login",
            ".bash_logout",
            ".bash_aliases",
            ".profile",
            ".zshrc",
            ".zshenv",
            ".zprofile",
            ".zlogin",
            ".zlogout",
            ".envrc",
            "PKGBUILD",
            ".env",
            ".env.*",
        ],
        extensions: &["bash", "sh", "zsh", "ksh"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Bass,
        indent: Indent::tab(),
        files: &[],
        extensions: &["bass"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Beancount,
        indent: Indent::tab(),
        files: &[],
        extensions: &["beancount"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Bibtex,
        indent: Indent::tab(),
        files: &[],
        extensions: &["bib"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Bitbake,
        indent: Indent::tab(),
        files: &[],
        extensions: &["bbappend", "bbclass"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Blade,
        indent: Indent::tab(),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::C,
        indent: Indent::space(4),
        files: &[],
        extensions: &["c", "h"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["function_definition", "struct_specifier"],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Clojure,
        indent: Indent::space(2),
        files: &[],
        extensions: &[
            "clj",
            "edn", // spellchecker:disable-line
            "cljs",
            "cljc",
            "cljd",
            "bb",
            "clj_kondo",
        ],
        comment: comment_properties!(";"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Cmake,
        indent: Indent::space(2),
        files: &["CMakeLists.txt"],
        extensions: &["cmake"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["function_definition"],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Comment,
        indent: Indent::tab(),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Cpp,
        indent: Indent::space(4),
        files: &[],
        extensions: &["cpp", "cxx", "cc", "c++", "hpp", "hxx", "hh", "h++"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &[
                "function_definition",
                "class_specifier",
                "struct_specifier",
            ],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Csharp,
        indent: Indent::space(2),
        files: &[],
        extensions: &["cs", "csx"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("c-sharp"),
            grammar_fn: Some("c-sharp"),
            query: Some("c-sharp"),
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &[
                "interface_declaration",
                "class_declaration",
                "enum_declaration",
                "struct_declaration",
                "record_declaration",
                "record_struct_declaration",
                "namespace_declaration",
                "constructor_declaration",
                "destructor_declaration",
                "method_declaration",
            ],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Css,
        indent: Indent::space(2),
        files: &[],
        extensions: &["css"],
        comment: comment_properties!("/*", "*/"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Cue,
        indent: Indent::tab(),
        files: &[],
        extensions: &["cue"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::D,
        indent: Indent::space(4),
        files: &[],
        extensions: &["d", "di", "dlang"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,
            multi_line_start: Some("/+"),
            multi_line_prefix: None,
            multi_line_end: Some("+/"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Dart,
        indent: Indent::space(2),
        files: &[],
        extensions: &["dart"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &["program", "class_definition"],
                &[
                    "program",
                    "import_or_export",
                    "comment",
                    "documentation_comment",
                ],
            ),
            sticky_headers: &["class_definition"],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Dhall,
        indent: Indent::tab(),
        files: &[],
        extensions: &["dhall"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Diff,
        indent: Indent::tab(),
        files: &[],
        extensions: &["diff", "patch"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Dockerfile,
        indent: Indent::space(2),
        files: &[
            "Dockerfile",
            "Containerfile",
            "Dockerfile.*",
            "Containerfile.*",
        ],
        extensions: &["containerfile", "dockerfile"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Dot,
        indent: Indent::tab(),
        files: &[],
        extensions: &["dot", "gv"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Elixir,
        indent: Indent::space(2),
        files: &[],
        extensions: &["ex", "exs", "eex", "heex", "sface"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["do_block"],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Elm,
        indent: Indent::space(4),
        files: &[],
        extensions: &["elm"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Erlang,
        indent: Indent::space(4),
        files: &[],
        extensions: &["erl", "hrl"],
        comment: comment_properties!("%"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Fish,
        indent: Indent::tab(),
        files: &[],
        extensions: &["fish"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Fluent,
        indent: Indent::tab(),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Forth,
        indent: Indent::tab(),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Fortran,
        indent: Indent::tab(),
        files: &[],
        extensions: &["f90", "f95", "f03", "f08", "f", "for"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::FSharp,
        indent: Indent::tab(),
        files: &[],
        extensions: &["fsx", "fsi"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Gitattributes,
        indent: Indent::tab(),
        files: &[".gitattributes"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties {
            query: Some("git-attributes"),
            ..TreeSitterProperties::DEFAULT
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::GitCommit,
        indent: Indent::tab(),
        files: &["COMMIT_EDITMSG", "MERGE_MSG", "TAG_EDITMSG"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::named("git-commit"),
    },
    SyntaxProperties {
        id: PlanckLanguage::GitConfig,
        indent: Indent::tab(),
        files: &[".gitconfig", ".git/config"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::named("git-config"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Gitignore,
        indent: Indent::tab(),
        files: &[".gitignore", ".dockerignore"],
        extensions: &[],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("gitignore"),
            grammar_fn: Some("gitignore"),
            query: Some("git-ignore"),
            ..TreeSitterProperties::DEFAULT
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::GitRebase,
        indent: Indent::tab(),
        files: &["git-rebase-todo"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::named("git-rebase"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Gleam,
        indent: Indent::tab(),
        files: &[],
        extensions: &["gleam"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Glimmer,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hbs"],
        comment: comment_properties!("{{!", "!}}"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Glsl,
        indent: Indent::space(2),
        files: &[],
        // spellchecker:off
        extensions: &[
            "glsl", "vs", "gs", "fs", "csh", "vsh", "gsh", "fsh", "cshader",
            "vshader", "gshader", "fshader", "comp", "vert", "geom", "frag", "tesc",
            "tese", "mesh", "task", "rgen", "rint", "rahit", "rchit", "rmiss",
            "rcall",
        ],
        // spellchecker:on
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Gn,
        indent: Indent::tab(),
        files: &[],
        extensions: &["gn", "gni"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Go,
        indent: Indent::tab(),
        files: &[],
        extensions: &["go"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &[
                    "source_file",
                    "type_declaration",
                    "type_spec",
                    "interface_type",
                    "method_spec_list",
                ],
                &["source_file", "comment", "line_comment"],
            ),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::GoMod,
        indent: Indent::tab(),
        files: &["go.mod"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::GoTemplate,
        indent: Indent::tab(),
        files: &[],
        extensions: &["gotmpl"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::named("gotmpl"),
    },
    SyntaxProperties {
        id: PlanckLanguage::GoWork,
        indent: Indent::tab(),
        files: &["go.work"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::GraphQl,
        indent: Indent::tab(),
        files: &[],
        extensions: &["graphql", "gql"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Groovy,
        indent: Indent::tab(),
        files: &["Jenkinsfile"],
        extensions: &["groovy", "gradle"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Hare,
        indent: Indent::space(8),
        files: &[],
        extensions: &["ha"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Haskell,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hs"],
        comment: comment_properties!("--"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Haxe,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hx"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Hcl,
        indent: Indent::space(2),
        files: &[],
        extensions: &["hcl", "tf"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    // Binary files open as a hex dump, colored line by line in
    // `style::hex_dump_line_styles` instead of by a grammar.
    SyntaxProperties {
        id: PlanckLanguage::HexDump,
        indent: Indent::space(2),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Hosts,
        indent: Indent::tab(),
        files: &["hosts"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Html,
        indent: Indent::space(4),
        files: &[],
        extensions: &["html", "htm"],
        comment: comment_properties!("<!--", "-->"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Ini,
        indent: Indent::tab(),
        files: &[".editorconfig"],
        extensions: &[
            "ini", "cfg", "conf", "service", "timer", "socket", "desktop",
        ],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Java,
        indent: Indent::space(4),
        files: &[],
        extensions: &["java"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Javascript,
        indent: Indent::space(2),
        files: &[],
        extensions: &["js", "cjs", "mjs"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Jinja2,
        indent: Indent::space(4),
        files: &[],
        extensions: &["jinja", "jinja2", "j2"],
        comment: comment_properties!("{#", "#}"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: Some("jinja"),
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Jsdoc,
        indent: Indent::tab(),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Json,
        indent: Indent::space(4),
        files: &["flake.lock"],
        extensions: &["json", "har", "jsonc"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Json5,
        indent: Indent::tab(),
        files: &[],
        extensions: &["json5"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Jsonnet,
        indent: Indent::tab(),
        files: &[],
        extensions: &["jsonnet", "libsonnet"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Jsx,
        indent: Indent::space(2),
        files: &[],
        extensions: &["jsx"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("javascript"),
            grammar_fn: Some("javascript"),
            query: Some("jsx"),
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Julia,
        indent: Indent::space(4),
        files: &[],
        extensions: &["julia", "jl"],
        comment: CommentProperties {
            single_line_start: Some("#"),
            single_line_end: None,
            multi_line_start: Some("#="),
            multi_line_prefix: None,
            multi_line_end: Some("=#"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Just,
        indent: Indent::tab(),
        files: &["justfile", "Justfile", ".justfile", ".Justfile"],
        extensions: &["just"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Kdl,
        indent: Indent::tab(),
        files: &[],
        extensions: &["kdl"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Kotlin,
        indent: Indent::space(2),
        files: &[],
        extensions: &["kt"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::KotlinBuildScript,
        indent: Indent::space(2),
        files: &[],
        extensions: &["kts"],
        comment: CommentProperties {
            single_line_start: Some("//"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties::named("kotlin"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Latex,
        indent: Indent::space(2),
        files: &[],
        extensions: &["tex"],
        comment: comment_properties!("%"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Ld,
        indent: Indent::tab(),
        files: &[],
        extensions: &["ld"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Llvm,
        indent: Indent::tab(),
        files: &[],
        extensions: &["ll"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::LlvmMir,
        indent: Indent::tab(),
        files: &[],
        extensions: &["mir"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::named("llvm-mir"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Log,
        indent: Indent::tab(),
        files: &["log.txt"],
        extensions: &["log"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Lua,
        indent: Indent::space(2),
        files: &[],
        extensions: &["lua"],
        comment: comment_properties!("--"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Make,
        indent: Indent::tab(),
        files: &["Makefile", "makefile", "GNUmakefile"],
        extensions: &["mk"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Markdown,
        indent: Indent::space(4),
        files: &[],
        extensions: &["md", "markdown"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::MarkdownInline,
        indent: Indent::space(4),
        // markdown inline is only used as an injection by the Markdown language
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties {
            grammar: Some("markdown_inline"),
            grammar_fn: Some("markdown_inline"),
            query: Some("markdown.inline"),
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Meson,
        indent: Indent::tab(),
        files: &["meson.build", "meson_options.txt", "meson.options"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Nasm,
        indent: Indent::tab(),
        files: &[],
        extensions: &["asm", "nasm"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Nix,
        indent: Indent::space(2),
        files: &[],
        extensions: &["nix"],
        comment: CommentProperties {
            single_line_start: Some("#"),
            single_line_end: None,

            multi_line_start: Some("/*"),
            multi_line_prefix: None,
            multi_line_end: Some("*/"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Nushell,
        indent: Indent::tab(),
        files: &[],
        extensions: &["nu"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::named("nu"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Ocaml,
        indent: Indent::space(2),
        files: &[],
        extensions: &["ml"],
        comment: CommentProperties {
            single_line_start: Some("(*"),
            single_line_end: Some("*)"),

            multi_line_start: Some("(*"),
            multi_line_prefix: Some("*"),
            multi_line_end: Some("*)"),
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::OcamlInterface,
        indent: Indent::space(2),
        files: &[],
        extensions: &["mli"],
        comment: comment_properties!("(*"),
        tree_sitter: TreeSitterProperties::named("ocaml-interface"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Odin,
        indent: Indent::tab(),
        files: &[],
        extensions: &["odin"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::OpenCl,
        indent: Indent::tab(),
        files: &[],
        extensions: &["cl"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Pascal,
        indent: Indent::tab(),
        files: &[],
        extensions: &["pas", "pp"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Passwd,
        indent: Indent::tab(),
        files: &["passwd"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Pem,
        indent: Indent::tab(),
        files: &[],
        extensions: &["pem", "crt"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Php,
        indent: Indent::space(2),
        files: &[],
        extensions: &["php"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &[
                    "program",
                    "class_declaration",
                    "trait_declaration",
                    "interface_declaration",
                    "declaration_list",
                    "method_declaration",
                    "function_declaration",
                ],
                &[
                    "program",
                    "php_tag",
                    "comment",
                    "namespace_definition",
                    "namespace_use_declaration",
                    "use_declaration",
                    "const_declaration",
                    "property_declaration",
                    "expression_statement",
                ],
            ),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Pkl,
        indent: Indent::tab(),
        files: &[],
        extensions: &["pkl"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::PowerShell,
        indent: Indent::space(4),
        files: &[],
        extensions: &["ps1", "psm1", "psd1", "ps1xml"],
        comment: CommentProperties {
            single_line_start: Some("#"),
            single_line_end: None,
            multi_line_start: Some("<#"),
            multi_line_end: Some("#>"),
            multi_line_prefix: None,
        },
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Prisma,
        indent: Indent::space(4),
        files: &[],
        extensions: &["prisma"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::ProtoBuf,
        indent: Indent::space(2),
        files: &[],
        extensions: &["proto"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Python,
        indent: Indent::space(4),
        files: &[],
        extensions: &["py", "pyi", "pyc", "pyd", "pyw"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &[
                    "source_file",
                    "module",
                    "class_definition",
                    "class",
                    "identifier",
                    "decorated_definition",
                    "block",
                ],
                &["source_file", "import_statement", "import_from_statement"],
            ),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Ql,
        indent: Indent::space(2),
        files: &[],
        extensions: &["ql"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::R,
        indent: Indent::space(2),
        files: &[],
        extensions: &["r"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Rcl,
        indent: Indent::tab(),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Regex,
        indent: Indent::tab(),
        files: &[],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Rego,
        indent: Indent::tab(),
        files: &[],
        extensions: &["rego"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Ron,
        indent: Indent::tab(),
        files: &[],
        extensions: &["ron"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Rst,
        indent: Indent::tab(),
        files: &[],
        extensions: &["rst"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Ruby,
        indent: Indent::space(2),
        files: &["Gemfile", "Rakefile"],
        extensions: &["rb", "gemspec", "rake"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (DEFAULT_CODE_GLANCE_LIST, DEFAULT_CODE_GLANCE_IGNORE_LIST),
            sticky_headers: &["module", "class", "method", "do_block"],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Rust,
        indent: Indent::space(4),
        files: &[],
        extensions: &["rs"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: None,
            grammar_fn: None,
            query: None,
            code_glance: (
                &["source_file", "impl_item", "trait_item", "declaration_list"],
                &["source_file", "use_declaration", "line_comment"],
            ),
            sticky_headers: &[
                "struct_item",
                "enum_item",
                "function_item",
                "impl_item",
            ],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Scala,
        indent: Indent::tab(),
        files: &[],
        extensions: &["scala", "sc", "sbt"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Scheme,
        indent: Indent::space(2),
        files: &[],
        extensions: &["scm", "ss"],
        comment: comment_properties!(";"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Scss,
        indent: Indent::space(2),
        files: &[],
        extensions: &["scss"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::ShellScript,
        indent: Indent::space(2),
        files: &[],
        extensions: &[],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::named("bash"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Smithy,
        indent: Indent::tab(),
        files: &[],
        extensions: &["smithy"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Sql,
        indent: Indent::space(2),
        files: &[],
        extensions: &["sql"],
        comment: comment_properties!("--"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::SshClientConfig,
        indent: Indent::tab(),
        files: &[".ssh/config", "ssh_config"],
        extensions: &[],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Strace,
        indent: Indent::tab(),
        files: &[],
        extensions: &["strace"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Svelte,
        indent: Indent::space(2),
        files: &[],
        extensions: &["svelte"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Sway,
        indent: Indent::tab(),
        files: &[],
        extensions: &["sw"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Swift,
        indent: Indent::space(2),
        files: &[],
        extensions: &["swift"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Tcl,
        indent: Indent::tab(),
        files: &[],
        extensions: &["tcl"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Toml,
        indent: Indent::space(2),
        files: &["Cargo.lock"],
        extensions: &["toml"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Tsx,
        indent: Indent::space(4),
        files: &[],
        extensions: &["tsx"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("tsx"),
            grammar_fn: Some("tsx"),
            query: Some("tsx"),
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Typescript,
        indent: Indent::space(4),
        files: &[],
        extensions: &["ts", "cts", "mts"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties {
            grammar: Some("typescript"),
            grammar_fn: Some("typescript"),
            query: Some("typescript"),
            code_glance: (&["source_file", "program"], &["source_file"]),
            sticky_headers: &[],
        },
    },
    SyntaxProperties {
        id: PlanckLanguage::Typst,
        indent: Indent::tab(),
        files: &[],
        extensions: &["typ"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Verilog,
        indent: Indent::tab(),
        files: &[],
        extensions: &["v", "sv", "svh"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Vue,
        indent: Indent::space(2),
        files: &[],
        extensions: &["vue"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Wasm,
        indent: Indent::space(4),
        files: &[],
        extensions: &["wat"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::named("wat"),
    },
    SyntaxProperties {
        id: PlanckLanguage::Wgsl,
        indent: Indent::space(4),
        files: &[],
        extensions: &["wgsl"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Wit,
        indent: Indent::space(4),
        files: &[],
        extensions: &["wit"],
        comment: comment_properties!(),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Xml,
        indent: Indent::space(4),
        files: &[],
        extensions: &["xml", "csproj", "svg", "xsd", "plist"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Yaml,
        indent: Indent::space(2),
        files: &[],
        extensions: &["yml", "yaml"],
        comment: comment_properties!("#"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
    SyntaxProperties {
        id: PlanckLanguage::Zig,
        indent: Indent::space(4),
        files: &[],
        extensions: &["zig"],
        comment: comment_properties!("//"),
        tree_sitter: TreeSitterProperties::DEFAULT,
    },
];

/// (language, weight, line-anchored regex) rules for `from_content`.
// shortcut: regex heuristics for common paste targets, add rules here when a language is misdetected
#[rustfmt::skip]
const CONTENT_RULES: &[(PlanckLanguage, u32, &str)] = {
    use PlanckLanguage::*;
    &[
        (Rust, 3, r"^\s*(pub(\([\w:]+\))?\s+)?(async\s+)?fn\s+\w+\s*[<(]"),
        (Rust, 3, r"^\s*use\s+(std|crate|super|self)::"),
        (Rust, 3, r"\blet\s+mut\s+\w+"),
        (Rust, 3, r"^\s*#\[(derive|cfg|test|allow)"),
        (Rust, 3, r"\b(println|format|vec)!\s*[(\[]"),
        (Rust, 2, r"^\s*impl(<[^>]*>)?\s+\w+"),
        (Rust, 1, r"^\s*(pub\s+)?(struct|enum|trait|mod)\s+\w+"),
        (Python, 3, r"^\s*(async\s+)?def\s+\w+\s*\(.*\)\s*(->\s*[^:]+)?:\s*$"),
        (Python, 3, r"^\s*class\s+\w+(\(.*\))?:\s*$"),
        (Python, 3, r"^\s*if\s+__name__\s*==\s*.__main__.\s*:"),
        (Python, 2, r"^\s*(from\s+[\w.]+\s+import\s+\w|import\s+[\w.]+(\s+as\s+\w+)?\s*$)"),
        (Python, 2, r"^\s*(elif\s.*|else|try|except(\s.*)?|finally)\s*:\s*$"),
        (Python, 1, r"\bself\.\w+"),
        (Javascript, 3, r"\brequire\(\s*['\x22]"),
        (Javascript, 3, r"^\s*(module\.)?exports\b"),
        (Javascript, 2, r"^\s*import\s+.*\s+from\s+['\x22]"),
        (Javascript, 2, r"\bfunction\s*\w*\s*\([^$]*$"),
        (Javascript, 2, r"\bconsole\.(log|error|warn)\("),
        (Javascript, 1, r"^\s*(export\s+)?(const|let|var)\s+\w+\s*="),
        (Javascript, 1, r"=>\s*[{(]?"),
        (Javascript, 1, r"\b(document|window)\.\w+"),
        (Typescript, 3, r"^\s*(export\s+)?(interface|type)\s+\w+(<[^>]*>)?\s*(=|\{|extends)"),
        (Typescript, 3, r":\s*(string|number|boolean|any|void|unknown|never)\b"),
        (Typescript, 2, r"\bas\s+(const|string|number|any|unknown)\b"),
        (Typescript, 1, r"^\s*(export\s+)?(enum|namespace|declare)\s+\w+"),
        (Go, 3, r"^package\s+\w+\s*$"),
        (Go, 3, r"^\s*func\s+(\(\w+\s+\*?\w+\)\s*)?\w+\s*\("),
        (Go, 3, r"^import\s+(\(|\x22[\w/.]+\x22)"),
        (Go, 3, r"\bfmt\.\w+\("),
        (Go, 3, r"^\s*type\s+\w+\s+(struct|interface)\s*\{"),
        (Go, 3, r"\berr\s*!=\s*nil\b"),
        (Go, 1, r":="),
        (C, 3, r"^\s*#include\s*[<\x22]"),
        (C, 2, r"\b(printf|malloc|free|sizeof)\s*\("),
        (C, 2, r"^\s*(static\s+)?(int|void|char|unsigned|long|double|float|size_t)\s+\**\w+\s*\("),
        (C, 1, r"^\s*#(define|ifndef|ifdef|endif|pragma)\b"),
        (C, 1, r"\w->\w"),
        (Cpp, 3, r"^\s*#include\s*<(iostream|string|vector|map|memory|algorithm)>"),
        (Cpp, 3, r"\bstd::(cout|cerr|endl|vector|unique_ptr|shared_ptr|make_unique|make_shared)\b"),
        (Cpp, 3, r"^\s*(template\s*<|namespace\s+\w+|using\s+namespace\s)"),
        (Cpp, 3, r"\bcout\s*<<|\bcin\s*>>"),
        (Java, 3, r"^\s*(public|private|protected)\s+(abstract\s+)?(static\s+)?(final\s+)?(class|interface|enum|record)\s+\w+"),
        (Java, 3, r"^\s*import\s+(static\s+)?[\w.]+(\.\*)?;\s*$"),
        (Java, 3, r"^\s*package\s+[\w.]+;\s*$"),
        (Java, 3, r"System\.out\.print"),
        (Java, 3, r"public\s+static\s+void\s+main"),
        (Java, 2, r"@Override\b"),
        (Csharp, 3, r"^\s*using\s+[\w.]+;\s*$"),
        (Csharp, 3, r"Console\.Write(Line)?\("),
        (Csharp, 3, r"\{\s*get;\s*(set;)?\s*\}"),
        (Csharp, 2, r"^\s*namespace\s+[\w.]+\s*(;|\{)?\s*$"),
        (Csharp, 2, r"\bvar\s+\w+\s*=\s*new\b"),
        (Php, 5, r"<\?php"),
        (Php, 3, r"\$this->"),
        (Php, 3, r"function\s+\w+\s*\([^)]*\$"),
        (Php, 3, r"^\s*use\s+[A-Z]\w*\\[\w\\]+;"),
        (Php, 1, r"\$\w+\s*="),
        (Ruby, 3, r"\b(puts|attr_accessor|attr_reader|attr_writer)\b"),
        (Ruby, 3, r"\.each\s+do\b|\bdo\s*\|\w+"),
        (Ruby, 3, r"^\s*#\s*frozen_string_literal"),
        (Ruby, 2, r"^\s*def\s+(self\.)?\w+[?!]?(\(.*\))?\s*$"),
        (Ruby, 2, r"^\s*require(_relative)?\s+['\x22]"),
        (Ruby, 2, r"^\s*(class|module)\s+[A-Z]\w*(\s*<\s*[A-Z][\w:]*)?\s*$"),
        (Ruby, 1, r"^\s*end\s*$"),
        (Bash, 3, r"^\s*(if|while|until)\s+\[\[?\s"),
        (Bash, 3, r"^\s*(fi|done|esac)\s*$"),
        (Bash, 3, r";\s*(then|do)\s*$"),
        (Bash, 3, r"^\s*set\s+-[euxo]"),
        (Bash, 1, r"^\s*(export\s+)?[A-Z_][A-Z0-9_]*="),
        (Bash, 1, r"\x22\$\{?\w+\}?\x22"),
        (Bash, 1, r"^\s*(echo|cd|mkdir|rm|export|source|sudo|apt|dnf|brew|cargo|npm|git)\s"),
        (Bash, 1, r"^\s*\w+\(\)\s*\{"),
        (Sql, 3, r"^\s*(SELECT|INSERT\s+INTO|UPDATE|DELETE\s+FROM|CREATE\s+(TABLE|INDEX|VIEW|DATABASE|SCHEMA|UNIQUE)|ALTER\s+TABLE|DROP\s+(TABLE|INDEX|VIEW))\b"),
        (Sql, 3, r"^\s*(select\s.*\sfrom\s|insert\s+into\s|create\s+table\s|delete\s+from\s|update\s+\w+\s+set\s)"),
        (Sql, 2, r"(?i)\b(varchar|primary\s+key|foreign\s+key|auto_increment)\b"),
        (Sql, 1, r"^\s*(FROM|WHERE|GROUP\s+BY|ORDER\s+BY|LEFT\s+JOIN|INNER\s+JOIN|JOIN|HAVING|LIMIT)\s"),
        (Sql, 1, r"\b(INTEGER|NOT NULL|BOOLEAN|TIMESTAMP)\b"),
        (Json, 2, r"\A\s*[\[{]"),
        (Json, 2, r"^\s*\x22[^\x22\n]+\x22\s*:\s*"),
        (Yaml, 2, r"^\s*- [\w.-]+:\s"),
        (Yaml, 2, r"^\s*(apiVersion|kind|metadata|spec|services|steps|jobs|runs-on|uses|image|env):"),
        (Yaml, 1, r"\A---\s*$"),
        (Yaml, 1, r"^\s*[\w.-]+:\s*(\||>)?\s*$"),
        (Yaml, 1, r"^\s*[\w.-]+:\s+\S"),
        (Toml, 3, r"^\s*\[\[[\w.-]+\]\]\s*$"),
        (Toml, 2, r"^\s*\[[\w.-]+\]\s*$"),
        (Toml, 2, r"^\s*[\w.-]+\s*=\s*(\x22.*\x22|'.*'|\d|true|false|\[|\{)"),
        (Html, 5, r"(?i)^\s*(<!doctype\s+html|<html\b)"),
        (Html, 1, r"(?i)<(div|span|p|a\s+href|head|body|script|style|ul|li|table|meta|link)\b"),
        (Html, 1, r"</\w+>"),
        (Xml, 5, r"^\s*<\?xml\b"),
        (Xml, 3, r"\bxmlns(:\w+)?="),
        (Css, 3, r"^\s*@(media|import|font-face|keyframes|charset)\b"),
        (Css, 2, r"^\s*([.#][\w-]+|:root|\*|body|html|h[1-6]|div|span|a|p|ul|li|table|img|button|input)([\s,>+~.#:\[][^{]*)?\{\s*$"),
        (Css, 1, r"^\s*[\w-]+\s*:\s*[^;{]+;\s*$"),
        (Css, 1, r"\b\d+(px|em|rem|vh|vw)\b|#[0-9a-fA-F]{3,6}\b"),
        (Markdown, 3, r"^\s*(```|~~~)"),
        (Markdown, 2, r"\[[^\]]+\]\((https?://|\.{0,2}/|#|[\w-]+\.(md|png|jpg|svg|html))"),
        (Markdown, 2, r"\*\*\w[^*\n]*\*\*"),
        (Markdown, 2, r"^\s*[-*]\s+\[[ xX]\]"),
        (Markdown, 1, r"^#{1,6}\s+\S"),
        (Markdown, 1, r"^\s*[-*+]\s+\S"),
        (Markdown, 1, r"^\s*\d+\.\s+\S"),
        (Markdown, 1, r"`[^`\n]+`"),
        (Markdown, 1, r"^>\s"),
        (Markdown, 1, r"^\s*\|.*\|\s*$"),
        (Diff, 5, r"^diff --git |^@@ -\d+(,\d+)? \+\d+(,\d+)? @@"),
        (Diff, 3, r"^(---|\+\+\+) [ab]/"),
        (Dockerfile, 3, r"^FROM\s+[\w./:-]+(\s+AS\s+\w+)?\s*$"),
        (Dockerfile, 2, r"^(RUN|COPY|ADD|WORKDIR|ENV|EXPOSE|CMD|ENTRYPOINT|ARG|LABEL|USER|VOLUME)\s"),
        (Lua, 3, r"^\s*local\s+(function\s+)?\w+"),
        (Lua, 3, r"^\s*function\s+\w+[.:]\w+\s*\("),
        (Lua, 1, r"\bthen\s*$"),
        (Lua, 1, r"^\s*end\s*$"),
        (Kotlin, 3, r"^\s*fun\s+(<[^>]+>\s*)?[\w.]+\s*\("),
        (Kotlin, 3, r"^\s*(data\s+class|companion\s+object|sealed\s+class|object\s+\w+)"),
        (Kotlin, 2, r"^\s*val\s+\w+"),
        (Kotlin, 2, r"^\s*var\s+\w+\s*:\s*\w"),
        (Kotlin, 1, r"\bprintln\("),
    ]
};

impl PlanckLanguage {
    const HIGHLIGHTS_INJECTIONS_FILE_NAME: &'static str = "injections.scm";
    const HIGHLIGHTS_QUERIES_FILE_NAME: &'static str = "highlights.scm";

    pub fn from_path(path: &Path) -> PlanckLanguage {
        Self::from_path_raw(path).unwrap_or(PlanckLanguage::PlainText)
    }

    pub fn from_path_raw(path: &Path) -> Option<PlanckLanguage> {
        let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let extension = path
            .extension()
            .and_then(|s| s.to_str().map(|s| s.to_lowercase()));
        // `files` entries: an exact name, `Name.*` for `Name.anything`, or
        // `dir/name` to match the end of the path.
        let file_matches = |f: &str| {
            if let Some(prefix) = f.strip_suffix(".*") {
                filename
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
            } else if f.contains('/') {
                path.ends_with(f)
            } else {
                f == filename
            }
        };
        // File names go first, so CMakeLists.txt isn't taken by `.txt`.
        // NOTE: This is a linear search.  It is assumed that this function
        // isn't called in any tight loop.
        LANGUAGES
            .iter()
            .find(|p| p.files.iter().any(|f| file_matches(f)))
            .or_else(|| {
                LANGUAGES.iter().find(|p| {
                    p.extensions
                        .iter()
                        .any(|e| Some(*e) == extension.as_deref())
                })
            })
            .map(|p| p.id)
    }

    /// Pick a language from the path and the first lines of the text.
    /// A shebang, `<?php` or diff header beats the extension; keyword scoring
    /// only fills in when the path says nothing (Untitled, `.txt`, unknown).
    pub fn detect(path: Option<&Path>, head: &str) -> PlanckLanguage {
        use PlanckLanguage::*;
        let by_path = path.and_then(Self::from_path_raw);
        if let Some(lang) = Self::from_first_line(head.lines().next().unwrap_or(""))
        {
            // `#!/usr/bin/env node` on top of a .ts file is still TypeScript.
            let js_in_ts = lang == Javascript
                && matches!(by_path, Some(Typescript | Tsx | Jsx));
            if !js_in_ts {
                return lang;
            }
        }
        match by_path {
            Some(lang) if lang != PlainText => lang,
            _ => Self::from_content(head).unwrap_or(PlainText),
        }
    }

    /// Markers on line 1 that say outright what the file is.
    fn from_first_line(line: &str) -> Option<PlanckLanguage> {
        static HEX_DUMP_ROW: Lazy<regex::Regex> = Lazy::new(|| {
            regex::Regex::new(r"^[0-9a-f]{8}  ([0-9a-f]{2} +){1,16} *\|.*\|$")
                .unwrap()
        });
        use PlanckLanguage::*;
        if HEX_DUMP_ROW.is_match(line) {
            return Some(HexDump);
        }
        if line.starts_with("<?php") {
            return Some(Php);
        }
        if line.starts_with("diff --git ") || line.starts_with("--- a/") {
            return Some(Diff);
        }
        let mut words = line.strip_prefix("#!")?.split_whitespace();
        let mut prog = words.next()?.rsplit('/').next()?;
        if prog == "env" {
            prog = words.find(|w| !w.starts_with('-') && !w.contains('='))?;
        }
        // python3.12 -> python, lua5.4 -> lua
        let prog = prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        Some(match prog {
            "python" | "pypy" => Python,
            "bash" | "sh" | "zsh" | "ksh" | "dash" => Bash,
            "fish" => Fish,
            "node" | "nodejs" => Javascript,
            "deno" | "bun" | "ts-node" | "tsx" => Typescript,
            "ruby" => Ruby,
            "php" => Php,
            "lua" | "luajit" => Lua,
            _ => return None,
        })
    }

    /// Weighted keyword scoring. The winner needs a minimum score and must
    /// beat the runner-up, so prose and ambiguous snippets stay plain text.
    fn from_content(head: &str) -> Option<PlanckLanguage> {
        use PlanckLanguage::*;
        const MIN_SCORE: u32 = 3;
        static RULES: Lazy<Vec<(PlanckLanguage, u32, regex::Regex)>> =
            Lazy::new(|| {
                CONTENT_RULES
                    .iter()
                    .map(|&(lang, weight, re)| {
                        (
                            lang,
                            weight,
                            regex::Regex::new(&format!("(?m){re}")).unwrap(),
                        )
                    })
                    .collect()
            });

        let mut scores: HashMap<PlanckLanguage, u32> = HashMap::new();
        for (lang, weight, re) in RULES.iter() {
            // At most 3 hits per rule, so one repeated line can't dominate.
            let hits = re.find_iter(head).take(3).count() as u32;
            if hits > 0 {
                *scores.entry(*lang).or_default() += weight * hits;
            }
        }
        // TypeScript and C++ code also scores as their base language.
        for (sup, base) in [(Typescript, Javascript), (Cpp, C)] {
            let b = scores.get(&base).copied().unwrap_or(0);
            if let Some(s) = scores.get_mut(&sup) {
                *s += b;
            }
        }

        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by_key(|r| std::cmp::Reverse(r.1));
        match ranked.as_slice() {
            [(lang, top), rest @ ..]
                if *top >= MIN_SCORE && rest.first().is_none_or(|r| r.1 < *top) =>
            {
                Some(*lang)
            }
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Option<PlanckLanguage> {
        match PlanckLanguage::from_str(name.to_lowercase().as_str()) {
            Ok(v) => Some(v),
            Err(e) => {
                event!(Level::DEBUG, "failed parsing `{name}` PlanckLanguage: {e}");
                None
            }
        }
    }

    pub fn languages() -> Vec<&'static str> {
        let mut langs = vec![];
        for l in LANGUAGES {
            // Get only languages with display name to hide inline grammars
            if let Some(lang) = strum::EnumMessage::get_message(&l.id) {
                langs.push(lang)
            }
        }
        langs
    }

    // NOTE: Instead of using `&LANGUAGES[*self as usize]` directly, the
    // `debug_assertion` gives better feedback should something has gone wrong
    // badly.
    fn properties(&self) -> &SyntaxProperties {
        let i = *self as usize;
        let l = &LANGUAGES[i];
        debug_assert!(
            l.id == *self,
            "LANGUAGES[{i}]: Setting::id mismatch: {:?} != {:?}",
            l.id,
            self
        );
        l
    }

    pub fn name(&self) -> &'static str {
        strum::EnumMessage::get_message(self).unwrap_or(self.into())
    }

    pub fn sticky_header_tags(&self) -> &[&'static str] {
        self.properties().tree_sitter.sticky_headers
    }

    pub fn comment_token(&self) -> &'static str {
        self.properties()
            .comment
            .single_line_start
            .unwrap_or_default()
    }

    pub fn indent_unit(&self) -> &str {
        self.properties().indent
    }

    fn get_grammar(&self) -> Option<tree_sitter::Language> {
        if matches!(self, PlanckLanguage::PlainText | PlanckLanguage::HexDump) {
            return None;
        }
        let grammar_name = self.grammar_name();
        let grammar_fn_name = self.grammar_fn_name();

        if let Some(grammars_dir) = Directory::grammars_directory() {
            match self::load_grammar(&grammar_name, &grammar_fn_name, &grammars_dir)
            {
                Ok(grammar) => {
                    return Some(grammar);
                }
                Err(err) => {
                    if self != &PlanckLanguage::PlainText {
                        tracing::error!("{:?} {:?}", self, err);
                    }
                }
            }
        };

        None
    }

    fn query_name(&self) -> String {
        self.properties()
            .tree_sitter
            .query
            .unwrap_or(self.properties().id.as_ref())
            .to_lowercase()
    }

    fn grammar_name(&self) -> String {
        self.properties()
            .tree_sitter
            .grammar
            .unwrap_or(self.properties().id.as_ref())
            .to_lowercase()
    }

    fn grammar_fn_name(&self) -> String {
        self.properties()
            .tree_sitter
            .grammar_fn
            .unwrap_or(self.properties().id.as_ref())
            .to_lowercase()
    }

    fn get_grammar_query(&self) -> (String, String) {
        let query_name = self.query_name();

        // Try reading highlights from user config dir
        if let Some(queries_dir) = Directory::queries_directory() {
            return (
                // Planck's own rules first, so they win over upstream ones.
                Self::extra_highlights(&query_name).to_string()
                    + &read_grammar_query(
                        &queries_dir,
                        &query_name,
                        Self::HIGHLIGHTS_QUERIES_FILE_NAME,
                    ),
                read_grammar_query(
                    &queries_dir,
                    &query_name,
                    Self::HIGHLIGHTS_INJECTIONS_FILE_NAME,
                ),
            );
        }

        ("".to_string(), "".to_string())
    }

    /// Highlight rules Planck adds on top of the upstream query of a language.
    fn extra_highlights(query_name: &str) -> &'static str {
        match query_name {
            "ada" => include_str!("../queries/ada/highlights.scm"),
            "bash" => include_str!("../queries/bash/highlights.scm"),
            "c" => include_str!("../queries/c/highlights.scm"),
            "comment" => include_str!("../queries/comment/highlights.scm"),
            "graphql" => include_str!("../queries/graphql/highlights.scm"),
            "json" => include_str!("../queries/json/highlights.scm"),
            "pkl" => include_str!("../queries/pkl/highlights.scm"),
            "rust" => include_str!("../queries/rust/highlights.scm"),
            _ => "",
        }
    }

    pub(crate) fn new_highlight_config(
        &self,
    ) -> Result<HighlightConfiguration, HighlightIssue> {
        let grammar = self.get_grammar().ok_or(HighlightIssue::NotAvailable)?;
        let (query, injection) = self.get_grammar_query();

        match HighlightConfiguration::new(grammar, &query, &injection, "") {
            Ok(x) => Ok(x),
            Err(x) => {
                let str = format!(
                    "Encountered {x:?} while trying to construct HighlightConfiguration for {}",
                    self.name()
                );
                event!(Level::ERROR, "{str}");
                Err(HighlightIssue::Error(str))
            }
        }
    }

    pub(crate) fn walk_tree(
        &self,
        cursor: &mut TreeCursor,
        normal_lines: &mut HashSet<usize>,
    ) {
        let (list, ignore_list) = self.properties().tree_sitter.code_glance;
        walk_tree(cursor, normal_lines, list, ignore_list);
    }
}

fn load_grammar(
    grammar_name: &str,
    grammar_fn_name: &str,
    path: &Path,
) -> Result<tree_sitter::Language, HighlightIssue> {
    let mut library_path = path.join(format!("libtree-sitter-{grammar_name}"));
    library_path.set_extension(std::env::consts::DLL_EXTENSION);

    if !library_path.exists() {
        event!(Level::WARN, "Grammar not found at: {library_path:?}");

        // Load backward compat libraries
        library_path = path.join(format!("tree-sitter-{grammar_name}"));
        library_path.set_extension(std::env::consts::DLL_EXTENSION);

        if !library_path.exists() {
            event!(Level::WARN, "Grammar not found at: {library_path:?}");
            return Err(HighlightIssue::Error("grammar not found".to_string()));
        }
    }

    event!(Level::DEBUG, "Loading grammar from user grammar dir");
    let library = match unsafe { libloading::Library::new(&library_path) } {
        Ok(v) => v,
        Err(e) => {
            let err = format!("Failed to load '{}': '{e}'", library_path.display());
            event!(Level::ERROR, err);
            return Err(HighlightIssue::Error(err));
        }
    };

    let language_fn_name =
        format!("tree_sitter_{}", grammar_fn_name.replace('-', "_"));
    event!(
        Level::DEBUG,
        "Loading grammar with address: '{language_fn_name}'"
    );
    let language = unsafe {
        let language_fn: libloading::Symbol<
            unsafe extern "C" fn() -> tree_sitter::Language,
        > = match library.get(language_fn_name.as_bytes()) {
            Ok(v) => v,
            Err(e) => {
                let err = format!("Failed to load '{language_fn_name}': '{e}'");
                event!(Level::ERROR, err);
                if let Some(e) = library.close().err() {
                    event!(Level::ERROR, "Failed to drop loaded library: {e}");
                };
                return Err(HighlightIssue::Error(err));
            }
        };
        language_fn()
    };
    std::mem::forget(library);

    Ok(language)
}

/// Walk an AST and determine which lines to include in the code glance.
///
/// Node types listed in `list` will be walked, along with their children. All
/// nodes encountered will be included, unless they are listed in `ignore_list`.
fn walk_tree(
    cursor: &mut TreeCursor,
    normal_lines: &mut HashSet<usize>,
    list: &[&str],
    ignore_list: &[&str],
) {
    let node = cursor.node();
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let kind = node.kind().trim();
    if !ignore_list.contains(&kind) && !kind.is_empty() {
        normal_lines.insert(start_pos.row);
        normal_lines.insert(end_pos.row);
    }

    if list.contains(&kind) && cursor.goto_first_child() {
        loop {
            walk_tree(cursor, normal_lines, list, ignore_list);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn add_bracket_pos(
    bracket_pos: &mut HashMap<usize, Vec<LineStyle>>,
    start_pos: Point,
    color: String,
) {
    let line_style = LineStyle {
        start: start_pos.column,
        end: start_pos.column + 1,
        style: Style {
            fg_color: Some(color),
        },
    };
    match bracket_pos.entry(start_pos.row) {
        Entry::Vacant(v) => _ = v.insert(vec![line_style]),
        Entry::Occupied(mut o) => o.get_mut().push(line_style),
    }
}

pub(crate) fn walk_tree_bracket_ast(
    cursor: &mut TreeCursor,
    level: &mut usize,
    counter: &mut usize,
    bracket_pos: &mut HashMap<usize, Vec<LineStyle>>,
    palette: &Vec<String>,
) {
    if cursor.node().kind().ends_with('(')
        || cursor.node().kind().ends_with('{')
        || cursor.node().kind().ends_with('[')
    {
        let row = cursor.node().end_position().row;
        let col = cursor.node().end_position().column - 1;
        let start_pos = Point::new(row, col);
        add_bracket_pos(
            bracket_pos,
            start_pos,
            palette.get(*level % palette.len()).unwrap().clone(),
        );
        *level += 1;
    } else if cursor.node().kind().ends_with(')')
        || cursor.node().kind().ends_with('}')
        || cursor.node().kind().ends_with(']')
    {
        let (new_level, overflow) = (*level).overflowing_sub(1);
        let row = cursor.node().end_position().row;
        let col = cursor.node().end_position().column - 1;
        let start_pos = Point::new(row, col);
        if overflow {
            add_bracket_pos(bracket_pos, start_pos, "bracket.unpaired".to_string());
        } else {
            *level = new_level;
            add_bracket_pos(
                bracket_pos,
                start_pos,
                palette.get(*level % palette.len()).unwrap().clone(),
            );
        }
    }
    *counter += 1;
    if cursor.goto_first_child() {
        loop {
            walk_tree_bracket_ast(cursor, level, counter, bracket_pos, palette);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn read_grammar_query(queries_dir: &Path, name: &str, kind: &str) -> String {
    static INHERITS_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r";+\s*inherits\s*:?\s*([a-z_,()-]+)\s*").unwrap());

    let file = queries_dir.join(name).join(kind);
    let query = std::fs::read_to_string(&file).unwrap_or_else(|err| {
        tracing::event!(
            tracing::Level::WARN,
            "Failed to read queries at: {file:?}, {err}"
        );
        String::new()
    });

    INHERITS_REGEX
        .replace_all(&query, |captures: &regex::Captures| {
            captures[1]
                .split(',')
                .fold(String::new(), |mut output, name| {
                    write!(
                        output,
                        "\n{}\n",
                        read_grammar_query(queries_dir, name, kind)
                    )
                    .unwrap();
                    output
                })
        })
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::PlanckLanguage;

    #[test]
    fn test_language_from_path() {
        let l = PlanckLanguage::from_path(&PathBuf::new().join("test.rs"));
        assert_eq!(l, PlanckLanguage::Rust);
        let l = PlanckLanguage::from_path(&PathBuf::from("/home/u/.zshrc"));
        assert_eq!(l, PlanckLanguage::Bash);

        use PlanckLanguage::*;
        for (path, want) in [
            ("/p/CMakeLists.txt", Cmake),
            ("/p/notes.txt", PlainText),
            ("/p/Dockerfile.dev", Dockerfile),
            ("/p/Containerfile.prod", Dockerfile),
            ("/p/Dockerfiles", PlainText),
            ("/p/.git/config", GitConfig),
            ("/home/u/.ssh/config", SshClientConfig),
            ("/p/config", PlainText),
            ("/p/.env.local", Bash),
            ("/p/Makefile", Make),
            ("/p/app.cs", Csharp),
            ("/p/x.fish", Fish),
            ("/p/build.gradle.kts", KotlinBuildScript),
        ] {
            assert_eq!(
                PlanckLanguage::from_path(&PathBuf::from(path)),
                want,
                "{path}"
            );
        }
    }

    #[test]
    fn test_languages_table_matches_enum() {
        // `properties()` indexes LANGUAGES by the enum value.
        for (i, l) in super::LANGUAGES.iter().enumerate() {
            assert_eq!(l.id as usize, i, "LANGUAGES[{i}] is {:?}", l.id);
        }
    }

    #[test]
    fn test_no_extension_or_file_claimed_twice() {
        let mut seen = std::collections::HashMap::new();
        for l in super::LANGUAGES {
            for key in l.extensions.iter().chain(l.files) {
                if let Some(first) = seen.insert(*key, l.id) {
                    panic!("{key:?} is claimed by {first:?} and {:?}", l.id);
                }
            }
        }
    }

    #[test]
    fn test_detect_untitled() {
        use PlanckLanguage::*;
        let cases = [
            (
                Rust,
                "use std::fmt;\n\nfn main() {\n    let mut x = 1;\n    println!(\"{x}\");\n}\n",
            ),
            (
                Python,
                "import os\n\ndef run(path):\n    if os.path.exists(path):\n        return True\n",
            ),
            (Python, "def f(x):\n    return x\n"),
            (
                Json,
                "{\n  \"name\": \"planck\",\n  \"version\": \"1.0\"\n}\n",
            ),
            (
                Yaml,
                "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
            ),
            (Sql, "SELECT id, name\nFROM users\nWHERE id = 1;\n"),
            (
                Go,
                "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n",
            ),
            (
                Html,
                "<!DOCTYPE html>\n<html>\n<body><div>hi</div></body>\n</html>\n",
            ),
            (Bash, "set -e\nfor f in *.txt; do\n  echo \"$f\"\ndone\n"),
            (
                Markdown,
                "# Title\n\nSome **bold** text and a [link](https://example.com).\n\n- one\n- two\n",
            ),
            (
                Diff,
                "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
            ),
            (
                Typescript,
                "interface User {\n  name: string;\n}\nconst u: User = { name: \"a\" };\n",
            ),
            (
                Javascript,
                "const fs = require('fs');\nmodule.exports = () => {};\n",
            ),
            (
                Cpp,
                "#include <iostream>\n\nint main() {\n    std::cout << \"hi\";\n}\n",
            ),
            (
                Dockerfile,
                "FROM rust:1.80\nWORKDIR /app\nCOPY . .\nRUN cargo build\n",
            ),
            (
                PlainText,
                "Meeting notes from Tuesday. We agreed to ship the release next week\nand to review the open issues before then.\n",
            ),
            (PlainText, ""),
        ];
        for (want, text) in cases {
            assert_eq!(PlanckLanguage::detect(None, text), want, "{text}");
        }
    }

    #[test]
    fn test_detect_overrides_path() {
        use PlanckLanguage::*;
        let cases = [
            ("notes.txt", "{\n  \"a\": 1,\n  \"b\": [true]\n}\n", Json),
            ("run.py", "#!/bin/bash\necho hi\n", Bash),
            (
                "app.ts",
                "#!/usr/bin/env node\nconsole.log(1);\n",
                Typescript,
            ),
            ("main.rs", "def f(x):\n    return x\n", Rust),
            ("main.rs", "#![allow(dead_code)]\nfn main() {}\n", Rust),
            ("script", "#!/usr/bin/env python3\nprint(1)\n", Python),
            (
                "deploy",
                "#!/usr/bin/env -S deno run\nconsole.log(1)\n",
                Typescript,
            ),
            (
                "demo.gif",
                "00000000  47 49 46 38 39 61 68 01 eb 00 f7 ff 00 19 16 27  |GIF89ah........'|\n",
                HexDump,
            ),
            // A hex dump row beats a known extension.
            (
                "data.json",
                "00000000  7b 0a                                            |{.|\n",
                HexDump,
            ),
        ];
        for (path, text, want) in cases {
            let got = PlanckLanguage::detect(Some(&PathBuf::from(path)), text);
            assert_eq!(got, want, "{path}");
        }
    }
}
