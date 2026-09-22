//! Tree-sitter syntax highlighting for rust, go, bash, python, c, json,
//! common lisp, javascript, typescript, markdown, toml, yaml, html, css,
//! lua, ruby, php, java, make, dockerfile, ini, diff, elisp, scheme, sql
//! and clojure.
//!
//! The highlighter keeps a per-character style grid (`line_styles`) with the
//! same shape as `Buffer::lines`, so lookups from the UI are plain index
//! accesses. `refresh` re-parses the whole buffer; files are small, so the
//! cost is a fraction of a millisecond.
//!
//! [`Stream`] is the same grammars with the opposite trade: it parses a
//! document that only ever grows (a streamed reply, a log tail) and keeps the
//! previous tree, so tree-sitter can reuse the unchanged prefix instead of
//! reparsing it. How much of the document that actually saves is the
//! grammar's decision rather than this crate's — see the note on `Stream`.

use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use tree_sitter::{
    InputEdit, Language, Parser, Point as TsPoint, Query, QueryCursor, Range, StreamingIterator,
    Tree,
};
use tree_sitter_language::LanguageFn;

use crate::buffer::{Buffer, Pos};

// tree-sitter-dockerfile is vendored (see `vendor/`) and compiled by
// `build.rs`: the crate binds tree-sitter 0.20, whose `language()` returns a
// type foreign to the 0.27 runtime rano uses, and referencing the crate
// drags the 0.20 C runtime into the link, colliding with 0.27's. The
// grammar itself is ABI-14, inside 0.27's supported range, so the C symbol
// is declared here directly.
unsafe extern "C" {
    fn tree_sitter_dockerfile() -> *const ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Go,
    Bash,
    Python,
    C,
    Json,
    CommonLisp,
    JavaScript,
    TypeScript,
    Tsx,
    Markdown,
    /// The inline half of markdown, for parsing the byte ranges the block
    /// grammar marks as inline content — the second half of markdown's
    /// two-grammar split (`tree-sitter-md`'s "Standalone usage"). Never
    /// detected from a path: a consumer pairs it with a [`Lang::Markdown`]
    /// [`Stream`] and feeds it included ranges.
    MarkdownInline,
    Toml,
    Yaml,
    Html,
    Css,
    Lua,
    Ruby,
    Php,
    Java,
    Make,
    Dockerfile,
    Ini,
    Diff,
    Elisp,
    Scheme,
    Sql,
    Clojure,
}

impl Lang {
    /// Language for an info-string token: what a markdown fence, a `lang=` attribute,
    /// or a `--lang` flag names a language with.
    ///
    /// **Not [`detect`]'s table**, and the difference is the point. A path says
    /// `foo.tsx` is TypeScript-with-JSX; a person writing ```` ```tsx ```` means tsx,
    /// and a person writing ```` ```sh ```` means bash — while `/bin/sh` is the one
    /// script that means "whichever shell is here". The two tables overlap and
    /// disagree (`mk` is Make as an extension and nothing as a token; `py` is Python in
    /// both; `c` is a letter that is only clear as a *language* when a fence names it),
    /// so this is its own mapping, drawn from what the sessions in this repository
    /// actually write in fences rather than from what extensions are called.
    ///
    /// Case-insensitive, and only the first word is read: an info string may carry a
    /// title, a directive or options after the language — ```` ```rust,ignore ````,
    /// ```` ```python title="x" ```` — and none of that names the grammar.
    ///
    /// `None` for a token rano has no grammar for, which is the common case for a
    /// model writing ```` ```text ```` or ```` ```console ````. The caller shows the
    /// text plain; guessing a grammar from the shape of the code is the invention a
    /// highlighter must not make.
    pub fn from_token(token: &str) -> Option<Lang> {
        let word = token
            .split(',')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("");
        match word.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Some(Lang::Rust),
            "go" | "golang" => Some(Lang::Go),
            "sh" | "bash" | "shell" | "zsh" => Some(Lang::Bash),
            "py" | "python" | "python2" | "python3" => Some(Lang::Python),
            // Bare `c` is a C fence about as often as it is a placeholder; both readings
            // want the C grammar, and a fence that meant something else names it.
            "c" | "h" => Some(Lang::C),
            "json" => Some(Lang::Json),
            "lisp" | "cl" | "commonlisp" | "common-lisp" => Some(Lang::CommonLisp),
            "elisp" | "emacs-lisp" | "el" => Some(Lang::Elisp),
            "js" | "jsx" | "javascript" | "mjs" | "node" => Some(Lang::JavaScript),
            "ts" | "typescript" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            // The inline grammar is not what a fence means; see `MarkdownInline`.
            "md" | "markdown" | "gfm" => Some(Lang::Markdown),
            "toml" => Some(Lang::Toml),
            "yaml" | "yml" => Some(Lang::Yaml),
            "html" | "htm" | "xhtml" | "xml" | "svg" => Some(Lang::Html),
            "css" => Some(Lang::Css),
            "lua" => Some(Lang::Lua),
            "rb" | "ruby" => Some(Lang::Ruby),
            "php" => Some(Lang::Php),
            "java" => Some(Lang::Java),
            "make" | "makefile" | "gnumakefile" => Some(Lang::Make),
            "dockerfile" | "docker" => Some(Lang::Dockerfile),
            "ini" | "cfg" | "conf" | "properties" | "editorconfig" => Some(Lang::Ini),
            "diff" | "patch" | "udiff" => Some(Lang::Diff),
            "scm" | "scheme" | "ss" | "rkt" => Some(Lang::Scheme),
            "sql" | "psql" | "mysql" | "plpgsql" => Some(Lang::Sql),
            "clj" | "cljs" | "cljc" | "edn" | "clojure" => Some(Lang::Clojure),
            _ => None,
        }
    }

    /// The language's name as a reader should see it: `"rust"`, `"typescript"`,
    /// `"dockerfile"`.
    ///
    /// Short and lowercase, matching what a person types in a fence rather than what an
    /// extension is called, so it can be shown beside the thing it names — a status bar,
    /// a fenced block's title — without a second table. [`Lang::from_token`] answers
    /// every one of these names, which is the property that keeps the two from drifting.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Go => "go",
            Lang::Bash => "bash",
            Lang::Python => "python",
            Lang::C => "c",
            Lang::Json => "json",
            Lang::CommonLisp => "commonlisp",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::Markdown => "markdown",
            Lang::MarkdownInline => "markdown-inline",
            Lang::Toml => "toml",
            Lang::Yaml => "yaml",
            Lang::Html => "html",
            Lang::Css => "css",
            Lang::Lua => "lua",
            Lang::Ruby => "ruby",
            Lang::Php => "php",
            Lang::Java => "java",
            Lang::Make => "make",
            Lang::Dockerfile => "dockerfile",
            Lang::Ini => "ini",
            Lang::Diff => "diff",
            Lang::Elisp => "elisp",
            Lang::Scheme => "scheme",
            Lang::Sql => "sql",
            Lang::Clojure => "clojure",
        }
    }

    pub(crate) fn language(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            Lang::CommonLisp => tree_sitter_commonlisp::LANGUAGE_COMMONLISP.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Markdown => tree_sitter_md::LANGUAGE.into(),
            Lang::MarkdownInline => tree_sitter_md::INLINE_LANGUAGE.into(),
            Lang::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Lang::Html => tree_sitter_html::LANGUAGE.into(),
            Lang::Css => tree_sitter_css::LANGUAGE.into(),
            Lang::Lua => tree_sitter_lua::LANGUAGE.into(),
            Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Make => tree_sitter_make::LANGUAGE.into(),
            Lang::Dockerfile => unsafe {
                Language::new(LanguageFn::from_raw(tree_sitter_dockerfile))
            },
            Lang::Ini => tree_sitter_ini::LANGUAGE.into(),
            Lang::Diff => tree_sitter_diff::LANGUAGE.into(),
            Lang::Elisp => tree_sitter_elisp::LANGUAGE.into(),
            Lang::Scheme => tree_sitter_scheme::LANGUAGE.into(),
            Lang::Sql => tree_sitter_sequel::LANGUAGE.into(),
            Lang::Clojure => tree_sitter_clojure_orchard::LANGUAGE.into(),
        }
    }

    fn query(self) -> &'static str {
        match self {
            Lang::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Lang::Go => tree_sitter_go::HIGHLIGHTS_QUERY,
            Lang::Bash => tree_sitter_bash::HIGHLIGHT_QUERY,
            Lang::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Lang::C => tree_sitter_c::HIGHLIGHT_QUERY,
            Lang::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Lang::CommonLisp => COMMONLISP_HIGHLIGHTS_QUERY,
            Lang::JavaScript => JS_HIGHLIGHTS_QUERY.as_str(),
            Lang::TypeScript | Lang::Tsx => TS_HIGHLIGHTS_QUERY.as_str(),
            Lang::Markdown => MARKDOWN_HIGHLIGHTS_QUERY,
            Lang::MarkdownInline => MARKDOWN_INLINE_HIGHLIGHTS_QUERY.as_str(),
            Lang::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Lang::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY,
            Lang::Html => tree_sitter_html::HIGHLIGHTS_QUERY,
            Lang::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
            Lang::Lua => tree_sitter_lua::HIGHLIGHTS_QUERY,
            Lang::Ruby => tree_sitter_ruby::HIGHLIGHTS_QUERY,
            Lang::Php => tree_sitter_php::HIGHLIGHTS_QUERY,
            Lang::Java => tree_sitter_java::HIGHLIGHTS_QUERY,
            Lang::Make => tree_sitter_make::HIGHLIGHTS_QUERY,
            Lang::Dockerfile => DOCKERFILE_HIGHLIGHTS_QUERY,
            Lang::Ini => tree_sitter_ini::HIGHLIGHTS_QUERY,
            Lang::Diff => tree_sitter_diff::HIGHLIGHTS_QUERY,
            Lang::Elisp => tree_sitter_elisp::HIGHLIGHTS_QUERY,
            Lang::Scheme => tree_sitter_scheme::HIGHLIGHTS_QUERY,
            Lang::Sql => tree_sitter_sequel::HIGHLIGHTS_QUERY,
            Lang::Clojure => CLOJURE_HIGHLIGHTS_QUERY.as_str(),
        }
    }
}

/// JavaScript = the crate's main query plus its JSX addendum; the crate
/// exports both but no combined one.
static JS_HIGHLIGHTS_QUERY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
    )
});

/// TypeScript and TSX = the JavaScript query (whose node names the TS/TSX
/// grammars are supersets of — it supplies the common keywords the
/// TypeScript crate's query assumes are already applied) plus the
/// TypeScript query with the TS-specific nodes.
static TS_HIGHLIGHTS_QUERY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY
    )
});

/// Clojure = the crate's query (literals, comments, reader macros) plus
/// rano's own supplement: the crate ships nothing for symbols, so call
/// heads and collection brackets would render uncoloured.
static CLOJURE_HIGHLIGHTS_QUERY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}",
        tree_sitter_clojure_orchard::HIGHLIGHTS_QUERY,
        r##"
; Plain symbols. `theme` has no "variable" entry, so the editor leaves
; them uncoloured; the more specific patterns below override this one.
(sym_lit) @variable

; The head of a list names the call. Defining and flow-control heads are
; keywords; any other head is a function. The anchor makes the pattern
; match the first child only, so arguments stay plain.
(list_lit . (sym_lit) @function)
((list_lit . (sym_lit) @keyword)
 (#any-of? @keyword
  "def" "defn" "defn-" "defmacro" "defonce" "defmulti" "defmethod"
  "defprotocol" "defrecord" "deftype" "definterface" "defstruct" "ns"
  "fn" "let" "letfn" "loop" "recur" "if" "if-let" "if-not" "if-some"
  "when" "when-let" "when-not" "when-first" "cond" "condp" "case" "do"
  "doto" "try" "catch" "finally" "throw" "quote" "var" "new" "set!"
  "binding" "with-open" "with-redefs" "doseq" "dotimes" "while"
  "declare" "require" "import" "use" "refer" "as->" "comment"))

; The name a function-ish definition binds: the symbol immediately after
; the head (defn name …). Anchored, so a docstring or the body is not
; swept in.
((list_lit . (sym_lit) @keyword . (sym_lit) @function)
 (#any-of? @keyword
  "defn" "defn-" "defmacro" "defonce" "defmulti" "defmethod"
  "defprotocol" "defrecord" "deftype" "definterface"))

; Collection brackets. The parens are left to the reader-macro captures
; above; quoting forms (` @ # etc.) are operators there.
["[" "]" "{" "}"] @punctuation.bracket
"##
    )
});

/// Dockerfile highlights, vendored verbatim from the grammar crate (it
/// ships the file but exports no const for it).
const DOCKERFILE_HIGHLIGHTS_QUERY: &str = r##"
[
	"FROM"
	"AS"
	"RUN"
	"CMD"
	"LABEL"
	"EXPOSE"
	"ENV"
	"ADD"
	"COPY"
	"ENTRYPOINT"
	"VOLUME"
	"USER"
	"WORKDIR"
	"ARG"
	"ONBUILD"
	"STOPSIGNAL"
	"HEALTHCHECK"
	"SHELL"
	"MAINTAINER"
	"CROSS_BUILD"
	(heredoc_marker)
	(heredoc_end)
] @keyword

[
	":"
	"@"
] @operator

(comment) @comment


(image_spec
	(image_tag
		":" @punctuation.special)
	(image_digest
		"@" @punctuation.special))

[
	(double_quoted_string)
	(single_quoted_string)
	(json_string)
	(heredoc_line)
] @string

(expansion
  [
	"$"
	"{"
	"}"
  ] @punctuation.special
) @none

((variable) @constant
 (#match? @constant "^[A-Z][A-Z_0-9]*$"))
"##;

/// Highlights query for Markdown — the BLOCK half, rano's own. The grammar
/// crate's block query uses nvim-treesitter capture names (`@text.title`, …)
/// that [`theme`] does not map, so the structural rules are spelled out here.
///
/// Inline content — emphasis, code spans, links, raw HTML — is not reachable
/// from this grammar at all: those are nodes of markdown's *other* grammar,
/// parsed over the ranges this tree marks as inline (see
/// [`markdown_inline_ranges`] and [`MARKDOWN_INLINE_HIGHLIGHTS_QUERY`]).
const MARKDOWN_HIGHLIGHTS_QUERY: &str = r##"
; Headings: markers purple, text yellow.
(atx_h1_marker) @keyword
(atx_h2_marker) @keyword
(atx_h3_marker) @keyword
(atx_h4_marker) @keyword
(atx_h5_marker) @keyword
(atx_h6_marker) @keyword
(setext_h1_underline) @keyword
(setext_h2_underline) @keyword
(atx_heading (inline) @type)
(setext_heading (paragraph) @type)

(thematic_break) @comment

; Structure markers: fences, quotes, tables, lists.
(fenced_code_block_delimiter) @punctuation.bracket
(block_quote_marker) @punctuation.bracket
(pipe_table_delimiter_row) @punctuation.bracket
(list_marker_plus) @punctuation.bracket
(list_marker_minus) @punctuation.bracket
(list_marker_star) @punctuation.bracket
(list_marker_dot) @punctuation.bracket
(list_marker_parenthesis) @punctuation.bracket
(task_list_marker_checked) @punctuation.bracket
(task_list_marker_unchecked) @punctuation.bracket

; Fenced-code info string (```yaml and friends).
(info_string) @attribute

; Code itself, so a fence reads as code and not as prose (nano colours the
; whole block; there is no injection grammar here to colour it by language).
(code_fence_content) @text.literal
(indented_code_block) @text.literal

; Raw HTML in the document.
(html_block) @tag

; Link reference definitions.
(link_destination) @property
(link_title) @string
(link_label) @string

; Escapes.
(entity_reference) @escape
(numeric_character_reference) @escape
(backslash_escape) @escape
"##;

/// The inline half of markdown — emphasis, code spans, links, raw HTML —
/// which no block-grammar query can reach: those are nodes of the OTHER
/// grammar, parsed over the ranges the block tree marks as inline content
/// (see [`markdown_inline_ranges`]).
///
/// `tree-sitter-md`'s own inline query plus the two node kinds it leaves
/// uncoloured, so a reader gets the distinctions nano's markdown mode draws:
/// `~~struck~~` and raw HTML tags. The names are nvim-treesitter's, which is
/// what `theme` maps.
static MARKDOWN_INLINE_HIGHLIGHTS_QUERY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}",
        tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
        r##"
; Not in the crate's query (nvim-treesitter leaves them to injections).
(strikethrough) @text.strike
(html_tag) @tag
"##
    )
});

/// Highlights query for Common Lisp. The grammar crate ships none, so this
/// is rano's own. Capture names are the standard tree-sitter set — [`theme`]
/// maps them, dotted ones by prefix — and only predicates the tree-sitter
/// crate itself evaluates are used (`#eq?`, `#match?`, `#any-of?`); the
/// neovim-only ones the upstream query leans on (`#lua-match?`, custom
/// predicates) would be silently ignored here, matching everything.
const COMMONLISP_HIGHLIGHTS_QUERY: &str = r##"
; Plain symbols. `theme` has no "variable" entry, so the editor leaves them
; uncoloured; the more specific patterns below override this one.
(sym_lit) @variable

(comment) @comment
(block_comment) @comment
(dis_expr) @comment

(str_lit) @string
(format_specifier) @escape

(num_lit) @number
(array_dimension) @number
(char_lit) @constant
(nil_lit) @constant.builtin
(kwd_lit) @constant

((sym_lit) @constant
  (#any-of? @constant "t" "T" "nil" "NIL"))

; Earmuffed constants (+pi+) and special variables (*standard-output*).
; Character classes, not escaped stars: the query parser eats `\*`.
((sym_lit) @constant
  (#match? @constant "^[+][^+]+[+]$"))
((sym_lit) @variable.builtin
  (#match? @variable.builtin "^[*][^*]+[*]$"))

; Lambda-list markers (&rest, &key, ...).
((sym_lit) @attribute
  (#match? @attribute "^[&]"))

; Defining forms: the head keyword and the name being defined.
(defun_keyword) @keyword
(defun_header
  function_name: (_) @function)

; LOOP keywords.
[
  (accumulation_verb)
  (for_clause_word)
  "always" "and" "as" "do" "else" "finally" "for" "if" "initially"
  "into" "loop" "never" "repeat" "return" "unless" "until" "when"
  "while" "with"
] @keyword

; Reader macros — quote, quasiquote, unquote, function reference — and
; the #+/#- read conditionals, coloured like preprocessor lines.
(quoting_lit "'" @escape)
(syn_quoting_lit "`" @escape)
(unquoting_lit "," @escape)
(unquote_splicing_lit ",@" @escape)
(var_quoting_lit "#'" @escape)
["#+" "#-"] @preproc

["(" ")" "."] @punctuation.bracket

"=" @operator
(list_lit
  .
  (sym_lit) @operator
  (#match? @operator "^([+*<>=/-]|<=|>=|/=)$"))

; Call heads: a curated subset of the standard's functions, macros and
; special operators. The nvim-treesitter query generates the full lists;
; this covers the everyday ones, lowercase — the convention in Lisp
; source.
(list_lit
  .
  (sym_lit) @function
  (#any-of? @function
    "1+" "1-" "abort" "abs" "acons" "acosh" "adjoin" "alpha-char-p"
    "alphanumericp" "and" "append" "apply" "apropos" "apropos-list" "aref"
    "arrayp" "ash" "asin" "asinh" "assert" "assoc" "assoc-if"
    "assoc-if-not" "atan" "atanh" "atom" "block" "boundp" "break" "butlast"
    "byte" "car" "cadr" "caar" "cdar" "cddr" "caddr" "cdddr" "cadddr"
    "caadr" "cdadr" "cddar" "cdaar" "caaar" "cddddr" "case" "ccase"
    "ceiling" "cerror" "change-class" "char" "char-code" "char-downcase"
    "char-int" "char-name" "char-upcase" "characterp" "check-type" "cis"
    "class-name" "class-of" "clear-input" "clear-output" "close" "clrhash"
    "code-char" "coerce" "compile" "compile-file" "complement" "complex"
    "complexp" "concatenate" "cond" "conjugate" "cons" "consp" "constantly"
    "constantp" "continue" "copy-alist" "copy-list" "copy-seq"
    "copy-symbol" "copy-tree" "cos" "cosh" "count" "count-if"
    "count-if-not" "ctypecase" "decf" "declaim" "declare" "defclass"
    "defconstant" "defgeneric" "define-compiler-macro" "define-condition"
    "define-method-combination" "define-modify-macro"
    "define-setf-expander" "define-symbol-macro" "defmacro" "defmethod"
    "defpackage" "defparameter" "defsetf" "defstruct" "deftype" "defun"
    "defvar" "delete" "delete-duplicates" "delete-file" "delete-if"
    "delete-if-not" "delete-package" "denominator" "describe"
    "destructuring-bind" "digit-char" "digit-char-p" "directory" "do"
    "do*" "do-all-symbols" "do-external-symbols" "do-symbols" "dolist"
    "documentation" "dotimes" "dpb" "eighth" "elt" "endp" "eq" "eql"
    "equal" "equalp" "error" "etypecase" "eval" "eval-when" "evenp" "every"
    "expt" "exp" "export" "fboundp" "fceiling" "ffloor" "fifth" "fill"
    "find" "find-all-symbols" "find-class" "find-if" "find-if-not"
    "find-method" "find-package" "find-restart" "find-symbol"
    "finish-output" "first" "float" "floatp" "floor" "fmakunbound"
    "force-output" "format" "fourth" "fresh-line" "fround" "ftruncate"
    "funcall" "function" "functionp" "gcd" "gensym" "gentemp" "get" "getf"
    "gethash" "get-properties" "get-setf-expansion" "get-universal-time"
    "get-internal-real-time" "get-internal-run-time" "go" "graphic-char-p"
    "handler-bind" "handler-case" "hash-table-p" "identity" "if"
    "ignore-errors" "imagpart" "import" "in-package" "incf"
    "initialize-instance" "input-stream-p" "inspect" "integerp" "intern"
    "invoke-debugger" "invoke-restart" "invoke-restart-interactively"
    "isqrt" "keywordp" "labels" "lambda" "last" "lcm" "ldb" "ldiff"
    "length" "let" "let*" "list" "list*" "list-all-packages" "list-length"
    "listp" "load" "load-time-value" "locally" "log" "logand" "logandc1"
    "logandc2" "logbitp" "logcount" "logeqv" "logior" "lognand" "lognor"
    "lognot" "logorc1" "logorc2" "logtest" "logxor" "loop" "loop-finish"
    "lower-case-p" "macroexpand" "macroexpand-1" "macrolet" "make-array"
    "make-condition" "make-hash-table" "make-instance" "make-list"
    "make-package" "make-pathname" "make-string" "make-string-input-stream"
    "make-string-output-stream" "make-symbol" "makunbound" "map" "mapc"
    "mapcan" "mapcar" "mapcon" "maphash" "map-into" "maplist" "max"
    "member" "member-if" "member-if-not" "merge" "merge-pathnames" "min"
    "minusp" "mismatch" "mod" "muffle-warning" "multiple-value-bind"
    "multiple-value-call" "multiple-value-list" "multiple-value-prog1"
    "multiple-value-setq" "nbutlast" "nconc" "next-method-p"
    "call-next-method" "nintersection" "ninth" "not" "notany" "notevery"
    "nreconc" "nreverse" "nset-difference" "nset-exclusive-or" "nsublis"
    "nsubst" "nsubstitute" "nsubstitute-if" "nsubstitute-if-not" "nth"
    "nthcdr" "nth-value" "null" "numberp" "numerator" "nunion" "oddp"
    "open" "or" "package-name" "packagep" "pairlis" "parse-integer"
    "parse-namestring" "pathname" "pathnamep" "peek-char" "phase" "plusp"
    "pop" "position" "position-if" "position-if-not" "prin1"
    "prin1-to-string" "princ" "princ-to-string" "print" "probe-file"
    "proclaim" "prog" "prog*" "prog1" "prog2" "progn" "progv" "provide"
    "psetf" "psetq" "push" "pushnew" "quote" "random" "rassoc" "rassoc-if"
    "rassoc-if-not" "rational" "rationalize" "rationalp" "read"
    "read-byte" "read-char" "read-char-no-hang" "read-delimited-list"
    "read-from-string" "read-line" "read-preserving-whitespace"
    "read-sequence" "realpart" "reduce" "reinitialize-instance" "rem"
    "remf" "remhash" "remove" "remove-duplicates" "remove-if"
    "remove-if-not" "remprop" "rename-file" "replace" "require" "rest"
    "restart-bind" "restart-case" "restart-name" "revappend" "reverse"
    "room" "rotatef" "round" "rplaca" "rplacd" "schar" "search" "second"
    "set" "set-difference" "set-exclusive-or" "setf" "setq" "seventh"
    "shadow" "shared-initialize" "shiftf" "signum" "signal" "sin" "sinh"
    "sixth" "sleep" "slot-boundp" "slot-exists-p" "slot-makunbound"
    "slot-value" "some" "sort" "special-operator-p" "sqrt" "stable-sort"
    "step" "store-value" "string" "string-capitalize" "string-downcase"
    "stringp" "string-left-trim" "string-right-trim" "string-trim"
    "string-upcase" "streamp" "sublis" "subseq" "subsetp" "subst"
    "substitute" "substitute-if" "substitute-if-not" "subtypep" "svref"
    "sxhash" "symbol-function" "symbol-macrolet" "symbol-name"
    "symbol-package" "symbol-plist" "symbol-value" "symbolp" "tagbody"
    "tan" "tanh" "tenth" "terpri" "the" "third" "throw" "time" "trace"
    "tree-equal" "truename" "truncate" "typecase" "type-of" "typep"
    "unexport" "unintern" "union" "unless" "unread-char" "unuse-package"
    "untrace" "unwind-protect" "upper-case-p" "use-package" "use-value"
    "values" "values-list" "vector" "vectorp" "vector-pop" "vector-push"
    "vector-push-extend" "warn" "wild-pathname-p" "with-accessors"
    "with-compilation-unit" "with-condition-restarts"
    "with-hash-table-iterator" "with-input-from-string" "with-open-file"
    "with-open-stream" "with-output-to-string" "with-package-iterator"
    "with-simple-restart" "with-slots" "with-standard-io-syntax" "write"
    "write-byte" "write-char" "write-line" "write-sequence"
    "write-string" "write-to-string" "yes-or-no-p" "y-or-n-p" "zerop"))
"##;

/// Map a file to its language: by extension first, then by the file name
/// (`Makefile`, `Dockerfile`, `.gitconfig` — extension-less conventions),
/// then by the shebang on line one (`#!/bin/sh`, `#!/usr/bin/env python3`)
/// for extension-less scripts (scratch buffers get none).
pub fn detect(name: Option<&Path>, first_line: Option<&str>) -> Option<Lang> {
    if let Some(ext) = name.and_then(|n| n.extension()).and_then(|e| e.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => return Some(Lang::Rust),
            "go" => return Some(Lang::Go),
            "sh" | "bash" => return Some(Lang::Bash),
            "py" | "pyw" => return Some(Lang::Python),
            "c" | "h" => return Some(Lang::C),
            "json" => return Some(Lang::Json),
            // .asd is an ASDF system definition, also Common Lisp.
            "lisp" | "cl" | "lsp" | "asd" => return Some(Lang::CommonLisp),
            "js" | "jsx" | "mjs" | "cjs" => return Some(Lang::JavaScript),
            "ts" | "mts" | "cts" => return Some(Lang::TypeScript),
            "tsx" => return Some(Lang::Tsx),
            "md" | "markdown" => return Some(Lang::Markdown),
            "toml" => return Some(Lang::Toml),
            "yaml" | "yml" => return Some(Lang::Yaml),
            "html" | "htm" | "xhtml" => return Some(Lang::Html),
            "css" => return Some(Lang::Css),
            "lua" => return Some(Lang::Lua),
            "rb" | "rake" => return Some(Lang::Ruby),
            "php" | "phtml" => return Some(Lang::Php),
            "java" => return Some(Lang::Java),
            "mk" => return Some(Lang::Make),
            "dockerfile" => return Some(Lang::Dockerfile),
            "ini" | "cfg" | "conf" | "properties" => return Some(Lang::Ini),
            "diff" | "patch" => return Some(Lang::Diff),
            "el" => return Some(Lang::Elisp),
            "scm" | "ss" => return Some(Lang::Scheme),
            "sql" => return Some(Lang::Sql),
            "clj" | "cljs" | "cljc" | "edn" => return Some(Lang::Clojure),
            _ => {}
        }
    }
    detect_filename(name).or_else(|| detect_shebang(first_line?))
}

/// A language's highlight query, **compiled once per process**.
///
/// `Query::new` parses the `.scm` and builds its automata, and it is the single most
/// expensive thing in this module: **8.7 ms** for Rust, measured 2026-09-20. That cost
/// is per *compilation*, not per use — a `Query` is immutable and `Send + Sync` — so
/// paying it once and sharing it is the whole difference between a renderer that starts
/// in milliseconds and one that starts in seconds.
///
/// It was not shared when `Stream::spans` first shipped, and the bill arrived as startup
/// time: `letibot`'s head renders every transcript row when it attaches, a row with a
/// code fence builds a `Stream`, and `spans` compiled a fresh query for each — eight
/// seconds of startup for forty compilations of four distinct queries. The same trap was
/// in `Highlighter`, which compiled per instance and so per diff excerpt. Both read this
/// now, and `a_query_is_compiled_once_per_process` is the regression.
///
/// `None` is cached too: a language whose query does not compile against its grammar
/// would otherwise retry on every call and re-parse the `.scm` each time.
fn highlight_query(lang: Lang) -> Option<Arc<Query>> {
    static CACHE: OnceLock<Mutex<HashMap<Lang, Option<Arc<Query>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(q) = guard.get(&lang) {
        return q.clone();
    }
    let compiled = Query::new(&lang.language(), lang.query())
        .ok()
        .map(Arc::new);
    guard.insert(lang, compiled.clone());
    compiled
}

/// Language for files known by NAME, not extension: `Makefile` (and
/// `Makefile.dev`, `GNUmakefile`), `Dockerfile` (and `Dockerfile.prod`),
/// and the dotfiles that are ini. Case-insensitive, the way these tools
/// accept them.
fn detect_filename(name: Option<&Path>) -> Option<Lang> {
    let file = name?.file_name()?.to_str()?.to_ascii_lowercase();
    if file == "makefile" || file == "gnumakefile" || file.starts_with("makefile.") {
        return Some(Lang::Make);
    }
    if file == "dockerfile" || file.starts_with("dockerfile.") {
        return Some(Lang::Dockerfile);
    }
    if matches!(
        file.as_str(),
        ".editorconfig" | ".gitconfig" | ".gitmodules"
    ) {
        return Some(Lang::Ini);
    }
    None
}

/// Language for a `#!` first line. Handles `#!/bin/sh`, `#! /bin/sh` and
/// `#!/usr/bin/env [-S …] python3` forms; interpreters we have no grammar
/// for (perl, awk, …) map to None.
fn detect_shebang(line: &str) -> Option<Lang> {
    let rest = line.strip_prefix("#!")?.trim_start();
    let mut words = rest.split_whitespace();
    let mut interp = words.next()?.rsplit('/').next()?;
    if interp == "env" {
        interp = words.find(|w| !w.starts_with('-'))?.rsplit('/').next()?;
    }
    match interp {
        "sh" | "bash" | "dash" | "ash" | "zsh" | "ksh" => Some(Lang::Bash),
        "python" | "python2" | "python3" | "pypy" | "pypy3" => Some(Lang::Python),
        // Common Lisp scripts, usually `#!/usr/bin/sbcl --script`.
        "sbcl" | "ccl" | "clisp" | "ecl" | "abcl" | "gcl" => Some(Lang::CommonLisp),
        "node" | "nodejs" => Some(Lang::JavaScript),
        "ruby" | "rake" => Some(Lang::Ruby),
        "php" => Some(Lang::Php),
        // Version-suffixed Lua interpreters: lua, lua5.4, luajit, …
        w if w.starts_with("lua") => Some(Lang::Lua),
        _ => None,
    }
}

/// Above this many bytes a buffer is highlighted by viewport window rather
/// than whole (see [`Highlighter::refresh_window`]).
///
/// Chosen so that no ordinary source file is affected: the largest files in a
/// real tree here are 0.7–0.9 MB (letibot's `app.rs`), and a full highlight of
/// one of those is a few milliseconds. Beyond this the whole-document cost
/// grows without bound — 7 MB measured 1.4 s per keystroke, and a 200 MB file
/// would be minutes per key — while the part a person can actually see is the
/// same few rows.
pub const LARGE_BUFFER: usize = 2 << 20;

/// A char window into a buffer: which rows, and — for the first and last row
/// only — which columns of them.
///
/// A row range is not enough to bound the work. minified JavaScript, one-line
/// JSON and a minified CSS bundle are a single row megabytes long, so `rows`
/// alone would put the whole file in the window. `cols` is what makes the
/// window a window in that case: `(char col the first row starts at, char col
/// the last row ends at)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Buffer rows covered, inclusive.
    pub rows: (usize, usize),
    /// `(start, end)` char columns, applied to the first and last row
    /// respectively. Ignored for a single row where start >= end.
    pub cols: (usize, usize),
}

impl Window {
    /// A window over whole rows.
    pub fn rows(first: usize, last: usize) -> Self {
        Self {
            rows: (first, last),
            cols: (0, usize::MAX),
        }
    }

    /// A window over one row's `[from, to)` char columns.
    pub fn cols(row: usize, from: usize, to: usize) -> Self {
        Self {
            rows: (row, row),
            cols: (from, to),
        }
    }

    /// The char range of `row` this window covers, as `(from, to)`.
    pub fn cols_of(&self, row: usize) -> (usize, usize) {
        let from = if row == self.rows.0 { self.cols.0 } else { 0 };
        let to = if row == self.rows.1 {
            self.cols.1
        } else {
            usize::MAX
        };
        (from, to)
    }

    /// How many characters the window can cover, for a buffer whose rows are
    /// `len_of`. Bounded by the buffer, not by the request.
    pub fn char_count(&self, n_rows: usize, len_of: impl Fn(usize) -> usize) -> usize {
        let last = self.rows.1.min(n_rows.saturating_sub(1));
        let mut total = 0usize;
        for row in self.rows.0..=last {
            let (from, to) = self.cols_of(row);
            let len = len_of(row);
            let from = from.min(len);
            let to = to.min(len).max(from);
            total += to - from + 1; // +1 for the newline joining rows
        }
        total
    }
}

/// The window's text, and the `(row, col)` offset of the window's first
/// character — which is what [`Highlighter::style_at`] adds back.
///
/// Shaped like the rows it came from: each covered row is followed by a
/// newline, so a partial first row still reads as a line to the grammar and
/// the row arithmetic in `for_each_capture` stays true.
fn window_text(buf: &Buffer, win: Window) -> (String, usize, usize) {
    let last = win.rows.1.min(buf.lines.len().saturating_sub(1));
    let first = win.rows.0.min(last);
    let mut s = String::with_capacity(win.char_count(buf.lines.len(), |r| buf.lines[r].len()) * 4);
    for row in first..=last {
        let (from, to) = win.cols_of(row);
        let line = &buf.lines[row];
        let from = from.min(line.len());
        let to = to.min(line.len()).max(from);
        s.extend(line[from..to].iter());
        s.push('\n');
    }
    let base_col = win.cols_of(first).0.min(buf.lines[first].len());
    (s, first, base_col)
}

/// Char count per `\n`-split line — `split('\n')`'s pieces exactly, so a trailing
/// newline yields a final empty row rather than being invisible.
fn line_char_counts(src: &str) -> Vec<usize> {
    src.split('\n').map(|l| l.chars().count()).collect()
}

/// Char index of byte `b` within the line that starts at `lo`.
///
/// Identity for an ASCII line, and a walk for the rare line that has a multibyte
/// character in it.
fn char_col(src: &str, lo: usize, b: usize, ascii: bool) -> usize {
    if ascii {
        b - lo
    } else {
        src[lo..b].chars().count()
    }
}

/// One-Dark-ish palette for a dark background.
fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// The byte ranges of a markdown block tree's inline content, **grouped by
/// the node that owns them**: one inner vec per `inline` / `pipe_table_cell`
/// node, each holding that node's ranges.
///
/// The grouping is the point, not an accident of implementation. The inline
/// grammar must be handed ONE NODE's ranges at a time, because it treats the
/// ranges it is given as a single concatenated stream: an emphasis delimiter
/// or a backtick run that cannot close inside its own node will happily pair
/// with one thousands of bytes later, in a different paragraph. That is not
/// hypothetical — measured on a 33 KB document, one unlucky `` ` ```console ` ``
/// span grew from 15 KB to 27 KB and painted everything between it and the
/// end of the document as a code span. `tree-sitter-md`'s own
/// `MarkdownParser` parses per node for the same reason.
///
/// Inside a node, the ranges are the node's range split around its NAMED
/// children: those are what the block grammar already parsed, so the inline
/// grammar must not re-parse them. The unnamed text between them is the
/// inline content proper.
fn markdown_inline_node_ranges(tree: &Tree) -> Vec<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "inline" || node.kind() == "pipe_table_cell" {
            let mut ranges = Vec::new();
            let mut at = node.start_byte();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor).filter(|c| c.is_named()) {
                if child.start_byte() > at {
                    ranges.push((at, child.start_byte()));
                }
                at = at.max(child.end_byte());
            }
            if at < node.end_byte() {
                ranges.push((at, node.end_byte()));
            }
            if !ranges.is_empty() {
                out.push(ranges);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    // Document order, so the captures of an earlier node are applied first
    // (they cannot overlap, but a stable order keeps the result reproducible).
    out.sort_unstable();
    out
}

/// Every inline range of the tree, flattened and sorted — "which bytes of
/// this document are inline content", which is what a caller pairing a second
/// grammar needs to know. The engine itself drives
/// [`markdown_inline_node_ranges`], because it must parse per node.
#[cfg(test)]
fn markdown_inline_ranges(tree: &Tree) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = markdown_inline_node_ranges(tree).concat();
    out.sort_unstable();
    out
}

fn theme(name: &str) -> Style {
    match name {
        "comment" => Style::default().fg(rgb(0x7f, 0x84, 0x8e)),
        "string" => Style::default().fg(rgb(0x98, 0xc3, 0x79)),
        "string.escape" | "escape" => Style::default().fg(rgb(0x56, 0xb6, 0xc2)),
        "number" | "constant" | "boolean" | "float" | "field" => {
            Style::default().fg(rgb(0xd1, 0x9a, 0x66))
        }
        "type" | "constructor" | "label" | "module" | "namespace" => {
            Style::default().fg(rgb(0xe5, 0xc0, 0x7b))
        }
        "attribute" => Style::default().fg(rgb(0x56, 0xb6, 0xc2)),
        "keyword" | "include" | "preproc" | "conditional" | "repeat" | "exception"
        | "storageclass" | "media" | "supports" | "keyframes" | "charset" | "import" => {
            Style::default().fg(rgb(0xc6, 0x78, 0xdd))
        }
        "operator" | "punctuation" => Style::default().fg(rgb(0xab, 0xbb, 0xbf)),
        "property" => Style::default().fg(rgb(0xd1, 0x9a, 0x66)),
        "function" | "method" => Style::default().fg(rgb(0x61, 0xaf, 0xef)),
        "variable.builtin" | "tag" | "error" => Style::default().fg(rgb(0xe0, 0x6c, 0x75)),
        // Markdown's own vocabulary (nvim-treesitter names, which is what the
        // inline query uses). The distinctions are nano's markdown mode's:
        // code and literals in cyan, emphasis and strong visually weighted,
        // links and URLs in the link colour, struck text dimmed.
        "text.literal" => Style::default().fg(rgb(0x56, 0xb6, 0xc2)),
        "text.emphasis" => Style::default()
            .fg(rgb(0x98, 0xc3, 0x79))
            .add_modifier(Modifier::ITALIC),
        "text.strong" => Style::default()
            .fg(rgb(0xe5, 0xc0, 0x7b))
            .add_modifier(Modifier::BOLD),
        "text.uri" => Style::default().fg(rgb(0x61, 0xaf, 0xef)),
        "text.reference" => Style::default().fg(rgb(0x61, 0xaf, 0xef)),
        "text.strike" => Style::default()
            .fg(rgb(0x7f, 0x84, 0x8e))
            .add_modifier(Modifier::CROSSED_OUT),
        // Dotted names we didn't match exactly fall back to their prefix
        // (e.g. "type.builtin" -> "type", "punctuation.bracket" -> "punctuation").
        _ => match name.split_once('.') {
            Some((prefix, _)) => theme(prefix),
            None => Style::default(),
        },
    }
}

pub struct Highlighter {
    parser: Parser,
    /// Markdown only: a second parser for the inline grammar, whose tree is
    /// parsed over the block tree's inline ranges. Kept so the inline pass
    /// does not allocate a parser per refresh.
    inline_parser: Parser,
    /// One entry per styled row. When `window` is set these are rows
    /// `[window, window + len)`, because a windowed refresh parses only that
    /// slice; the slice's own line 0 is `line_styles[0]`.
    line_styles: Vec<Vec<Style>>,
    /// The char window the grid covers, when it covers a window rather than
    /// the whole buffer, plus the `(row, col)` offset of the grid's first cell.
    window: Option<(Window, usize, usize)>,
    /// The last successful parse, kept so syntax errors can be surfaced
    /// without a language server (see [`Highlighter::syntax_errors`]).
    tree: Option<Tree>,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            inline_parser: Parser::new(),
            line_styles: Vec::new(),
            window: None,
            tree: None,
        }
    }

    /// Re-parse the buffer and rebuild the style grid. No-op for scratch
    /// buffers (no file name, no language).
    ///
    /// Re-highlights the WHOLE buffer. On a large file that is O(document) in
    /// both the parse and the capture walk, so the editor does not call this
    /// per keystroke — it calls [`Self::refresh_window`] for anything above
    /// [`crate::syntax::LARGE_BUFFER`]. This is what the export path and the
    /// tests use, where colouring everything is the point.
    pub fn refresh(&mut self, buf: &Buffer) {
        self.refresh_rows(buf, None);
    }

    /// Highlight only a byte window of the buffer, addressed as a char range.
    ///
    /// Cost is proportional to the window, not to the document. Before this, a
    /// keystroke on a 7 MB file cost ~1.4 s when everything was highlighted
    /// (723 ms re-parsing the document, ~600 ms walking every capture in it);
    /// a viewport window is in the microsecond range.
    ///
    /// The window is a CHAR range rather than a row range because a row can be
    /// megabytes long: minified JavaScript and one-line JSON are single lines,
    /// and a row window over one of those is the whole file. `cols` bounds the
    /// first and last rows, so the window is still the few thousand
    /// characters a person can see when the document is one enormous line.
    ///
    /// The trade is stated rather than hidden: a construct opening OUTSIDE the
    /// window and closing inside it is coloured as though it opened inside (a
    /// block comment continued from above is the visible case), and a windowed
    /// highlight produces no tree-sitter syntax diagnostics. Callers pass a
    /// margin so those edges are off screen; see [`crate::editor`]'s
    /// `HIGHLIGHT_MARGIN`.
    pub fn refresh_window(&mut self, buf: &Buffer, win: Window) {
        let last = win.rows.1.min(buf.lines.len().saturating_sub(1));
        if win.rows.0 > last {
            return;
        }
        self.refresh_rows(buf, Some(win));
    }

    /// The char range [`Self::style_at`] can answer for, when it is a window.
    pub fn styled_window(&self) -> Option<Window> {
        self.window.map(|(w, _, _)| w)
    }

    fn refresh_rows(&mut self, buf: &Buffer, win: Option<Window>) {
        let first_line = buf.lines.first().map(|l| l.iter().collect::<String>());
        let Some(lang) = detect(buf.name.as_deref(), first_line.as_deref()) else {
            self.clear();
            return;
        };

        // Only the window's characters are materialised. `buf.text()` copies
        // the whole document — 200 MB of memcpy per keystroke at 200 MB — so a
        // window is sliced first. The tree, the style grid and every index
        // below are then relative to the slice, and `style_at` adds the offset
        // back; nothing else has to know.
        let (source, base_row, base_col) = match win {
            None => (buf.text(), 0usize, 0usize),
            Some(w) => {
                let (text, base_row, base_col) = window_text(buf, w);
                (text, base_row, base_col)
            }
        };
        // What was actually parsed, after the buffer's own bounds clamped it.
        let parsed = win.map(|w| Window {
            rows: (w.rows.0, w.rows.1.min(buf.lines.len().saturating_sub(1))),
            cols: w.cols,
        });
        if self.parser.set_language(&lang.language()).is_err() {
            self.clear();
            return;
        }

        // Always a fresh parse, of the window. Reusing the previous tree for
        // incremental parsing leaks byte offsets from the old (possibly
        // longer) source into the new tree, which panics inside tree-sitter
        // when a line is shortened. Re-parsing a window is fast at any file
        // size; re-parsing a whole document is not.
        let Some(tree) = self.parser.parse(source.as_bytes(), None) else {
            self.clear();
            return;
        };
        self.tree = Some(tree);

        // Process-wide rather than per instance: a `Highlighter` is built per diff
        // excerpt by `letibot`'s two-panel view, so a per-instance query meant one 8.7 ms
        // compilation per excerpt. See [`highlight_query`].
        let Some(query) = highlight_query(lang) else {
            self.clear();
            return;
        };

        let tree = self.tree.as_ref().unwrap();
        let mut grid = Self::build_styles(&source, tree, &query);
        // Markdown is two grammars. The block pass above coloured structure;
        // emphasis, code spans and links are nodes of the *inline* grammar
        // and can only come from a second pass over the ranges the block tree
        // marked. Both overlay one grid, in that order.
        if lang == Lang::Markdown {
            let block = self.tree.clone().expect("just set");
            self.markdown_inline_pass(&source, &block, |slice, base_row, t, q| {
                Self::apply_styles(slice, t, q, base_row, &mut grid)
            });
        }
        self.line_styles = grid;
        self.window = parsed.map(|w| (w, base_row, base_col));
    }

    /// Drop everything this highlighter knows. The state after a failure, and
    /// after a buffer with no detectable language.
    fn clear(&mut self) {
        self.line_styles.clear();
        self.tree = None;
        self.window = None;
    }

    /// Run the inline half of markdown's highlighting: for each inline node,
    /// parse the inline grammar over THAT NODE's ranges and hand the caller
    /// the resulting tree.
    ///
    /// One parse per node, not one parse over all the ranges at once. The
    /// inline grammar sees the ranges it is given as one concatenated stream,
    /// so a delimiter that cannot close inside its own node would pair with
    /// one in a later paragraph and swallow the document between them
    /// (measured: a 12 KB code span out of a stray `` ` ```console ` ``). Per
    /// node, the worst case is confined to the node — which is what
    /// `tree-sitter-md`'s own parser does, and why.
    fn markdown_inline_pass<F: FnMut(&str, usize, &Tree, &Query)>(
        &mut self,
        src: &str,
        block: &Tree,
        mut apply: F,
    ) {
        let Some(query) = highlight_query(Lang::MarkdownInline) else {
            return;
        };
        if self
            .inline_parser
            .set_language(&Lang::MarkdownInline.language())
            .is_err()
        {
            return;
        }
        // Line starts for the whole source, computed ONCE: the per-node row
        // base below needs them, and recomputing per node made this quadratic.
        let mut line_starts = vec![0usize];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let row_of = |byte: usize| {
            line_starts
                .partition_point(|&o| o <= byte)
                .saturating_sub(1)
        };

        for group in markdown_inline_node_ranges(block) {
            // ONE NODE, and only that node's bytes are handed to the parser.
            //
            // Passing the whole source with the node's ranges as
            // `set_included_ranges` is what the API suggests and it is
            // quadratic: tree-sitter has to walk the excluded input to reach
            // each range, so a 2,400-paragraph document cost 9.8× for 4× the
            // text (measured 2026-09-22, caught by
            // `the_markdown_two_pass_is_linear_in_the_document`). The slice
            // below makes each parse cost its own node.
            //
            // The slice is grown to FULL rows so the row base is a whole-row
            // offset and a node's columns stay aligned with the grid's.
            let lo = group[0].0;
            let hi = group[group.len() - 1].1;
            let base_row = row_of(lo);
            let slice_lo = line_starts[base_row];
            let slice_hi = line_starts
                .get(row_of(hi.saturating_sub(1)) + 1)
                .copied()
                .unwrap_or(src.len())
                .max(hi);
            let slice = &src[slice_lo..slice_hi];
            let ranges: Vec<Range> = group
                .iter()
                .map(|&(s, e)| Range {
                    start_byte: s.saturating_sub(slice_lo),
                    end_byte: e.saturating_sub(slice_lo).min(slice.len()),
                    start_point: point_of(slice, s.saturating_sub(slice_lo)).into(),
                    end_point: point_of(slice, e.saturating_sub(slice_lo).min(slice.len())).into(),
                })
                .collect();
            if self.inline_parser.set_included_ranges(&ranges).is_err() {
                continue;
            }
            if let Some(tree) = self.inline_parser.parse(slice.as_bytes(), None) {
                apply(slice, base_row, &tree, &query);
            }
        }
    }

    /// Style for the character at `p`, if any capture colors it.
    pub fn style_at(&self, p: Pos) -> Option<Style> {
        // A windowed grid is indexed from the window's own first character, so
        // map the absolute position down: the row by the window's first row,
        // and — only on that first row — the column by the window's first
        // column. Outside the window there is no answer, which the caller
        // reads as "uncoloured" — see [`Self::refresh_window`].
        let (row_idx, col_idx) = match self.window {
            Some((_, base_row, base_col)) => {
                let row = p.row.checked_sub(base_row)?;
                let col = if row == 0 {
                    p.col.checked_sub(base_col)?
                } else {
                    p.col
                };
                (row, col)
            }
            None => (p.row, p.col),
        };
        let row = self.line_styles.get(row_idx)?;
        let st = row.get(col_idx)?;
        if *st == Style::default() {
            None
        } else {
            Some(*st)
        }
    }

    /// Syntax classes for raw text that never touches an editor buffer: one
    /// row per `\n`-split line, one entry per character — the tree-sitter
    /// capture name that colours it (`"keyword"`, `"function"`, …), or
    /// `None` where no capture applies.
    ///
    /// This is the engine without a palette. The caller owns the mapping
    /// from capture names to colours, so the language is passed explicitly
    /// instead of being detected from a buffer name — [`detect`] is still
    /// the way to get one from a path. The query is cached per language
    /// exactly as [`Self::refresh`] caches it, and the parse tree is kept
    /// as the last parse; the editor's own style grid is **not** touched,
    /// so a `Highlighter` shared between this and a buffer would show stale
    /// [`Self::style_at`] answers — give the embedder its own instance.
    ///
    /// Empty on failure (unknown language, query failed to compile, parse
    /// failed): the caller reads a missing row as "uncoloured", which is
    /// the same thing it does with a `None` cell.
    pub fn classes(&mut self, src: &str, lang: Lang) -> Vec<Vec<Option<String>>> {
        let Some(tree) = (|| {
            self.parser.set_language(&lang.language()).ok()?;
            self.parser.parse(src.as_bytes(), None)
        })() else {
            return Vec::new();
        };
        // Shared process-wide, not per instance: `letibot`'s two-panel diff builds a
        // `Highlighter` per excerpt, so a per-instance query was one 8.7 ms compilation
        // per excerpt — the same trap `Stream::spans` was in.
        let Some(query) = highlight_query(lang) else {
            return Vec::new();
        };
        self.tree = Some(tree);
        let mut grid = Self::build_classes(src, self.tree.as_ref().unwrap(), &query);
        // The same two-grammar split as `refresh`, so an embedder reading
        // capture names sees the inline constructs too.
        if lang == Lang::Markdown {
            let block = self.tree.clone().expect("just set");
            self.markdown_inline_pass(src, &block, |slice, base_row, t, q| {
                Self::apply_classes(slice, t, q, base_row, &mut grid)
            });
        }
        grid
    }

    /// Syntax errors from the last parse as `(line, col, end_col, message)`
    /// in char columns, from `ERROR` nodes and missing nodes. Empty for
    /// scratch buffers and clean parses.
    pub fn syntax_errors(&self, lines: &[Vec<char>]) -> Vec<(usize, usize, usize, String)> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        // A windowed tree is parsed from a slice, so its rows are slice-relative
        // and its `lines` do not match the caller's. Offsetting them here would
        // need the slice's own lines for the column arithmetic, so a windowed
        // highlight simply reports no syntax diagnostics: the LSP's still
        // arrive, and a file big enough to need a window is one where per-row
        // tree-sitter errors are not what a reader is looking at.
        if self.window.is_some() {
            return Vec::new();
        }
        let root = tree.root_node();
        if !root.has_error() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut stack = vec![(root, false)];
        while let Some((node, in_error)) = stack.pop() {
            let is_error = node.is_error() && !in_error;
            if node.is_missing() {
                // Missing nodes are zero-width insertion points; the caller
                // widens them to one visible column.
                let (l, c) = char_pos(lines, node.start_position());
                out.push((l, c, c, format!("missing {}", node.kind())));
                continue;
            }
            if is_error {
                let (l, c) = char_pos(lines, node.start_position());
                let (el, ec) = char_pos(lines, node.end_position());
                let end = if el == l { ec } else { c + 1 };
                out.push((l, c, end.max(c + 1), "syntax error".to_string()));
            }
            let mut cursor = node.walk();
            if cursor.goto_first_child() {
                loop {
                    stack.push((cursor.node(), in_error || is_error));
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn build_styles(src: &str, tree: &Tree, query: &Query) -> Vec<Vec<Style>> {
        let mut line_styles: Vec<Vec<Style>> = line_char_counts(src)
            .into_iter()
            .map(|n| vec![Style::default(); n])
            .collect();
        Self::apply_styles(src, tree, query, 0, &mut line_styles);
        line_styles
    }

    /// Overlay one tree's captures on an existing style grid. Called twice for
    /// markdown — the block tree, then the inline tree over the same cells —
    /// which is what makes the second pass compose instead of fight.
    fn apply_styles(
        src: &str,
        tree: &Tree,
        query: &Query,
        row_base: usize,
        line_styles: &mut [Vec<Style>],
    ) {
        Self::for_each_capture(src, tree, query, row_base, |r, cs, name| {
            let style = theme(name);
            if style == Style::default() {
                return;
            }
            for cell in &mut line_styles[r][cs] {
                *cell = style;
            }
        });
    }

    /// The capture grid without a palette: one row per line, one entry per
    /// character — the tree-sitter capture name that covers it, or `None`.
    ///
    /// [`Self::build_styles`] is this plus rano's [`theme`]; a caller that
    /// owns its palette (another crate embedding the engine) wants the
    /// names instead, because capture → colour is a decision about the
    /// terminal, not about the grammar.
    fn build_classes(src: &str, tree: &Tree, query: &Query) -> Vec<Vec<Option<String>>> {
        let mut grid: Vec<Vec<Option<String>>> = line_char_counts(src)
            .into_iter()
            .map(|n| vec![None; n])
            .collect();
        Self::apply_classes(src, tree, query, 0, &mut grid);
        grid
    }

    /// Overlay one tree's captures on an existing class grid.
    fn apply_classes(
        src: &str,
        tree: &Tree,
        query: &Query,
        row_base: usize,
        grid: &mut [Vec<Option<String>>],
    ) {
        Self::for_each_capture(src, tree, query, row_base, |r, cs, name| {
            for cell in &mut grid[r][cs] {
                *cell = Some(name.to_string());
            }
        });
    }

    /// Walk every query capture and hand the caller the cells it covers as
    /// `(row, char_range, capture_name)`. Ranges are half-open and within
    /// the row. Later captures overwrite earlier ones cell by cell, which
    /// is the order [`Self::build_styles`] has always had; what a capture
    /// *means* is the callback's decision, which is why the default-styled
    /// skip lives in [`Self::build_styles`] and not here.
    ///
    /// Takes `src`, **not** a per-line `Vec<Vec<char>>`. Building that was one
    /// allocation per line per call and it dominated the walk: measured
    /// 2026-09-20, ~1.9 µs per line against ~0.05 µs per byte, so 7.7 KB in 700
    /// lines cost 1.34 ms per walk while 9 KB in one line cost 0.46 ms. A line
    /// that is pure ASCII — nearly every line of source — needs no conversion
    /// at all, because byte index and char index are the same number.
    fn for_each_capture(
        src: &str,
        tree: &Tree,
        query: &Query,
        row_base: usize,
        mut f: impl FnMut(usize, std::ops::Range<usize>, &str),
    ) {
        // Byte ranges of each line — `split('\n')`'s pieces exactly, including the
        // empty one after a trailing newline, which is a row a caller may index.
        let mut bounds: Vec<(usize, usize)> = Vec::new();
        let mut start = 0usize;
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                bounds.push((start, i));
                start = i + 1;
            }
        }
        bounds.push((start, src.len()));
        let starts: Vec<usize> = bounds.iter().map(|(s, _)| *s).collect();
        // A line with no multibyte character converts byte offset to char index by
        // subtraction, which is nearly every line of source.
        let ascii: Vec<bool> = bounds.iter().map(|(s, e)| src[*s..*e].is_ascii()).collect();

        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut caps = cursor.captures(query, tree.root_node(), src.as_bytes());
        // A row cursor that only ever moves FORWARD. Captures arrive in
        // document order, so the line holding each one is at or after the line
        // holding the previous one — a binary search per capture was
        // `#captures × log(#lines)` random accesses into a multi-megabyte
        // array, and on a large file that was the whole cost of a keystroke
        // (measured 2026-09-22: ~2.2 s of a 3.0 s keystroke on a 7 MB Rust
        // file, ~1.8 s of 2.3 s on a 4 MB single-line one).
        let mut row = 0usize;
        while let Some((m, i)) = caps.next() {
            let cap = m.captures()[*i];
            let name = names.get(cap.index as usize).copied().unwrap_or("");
            let (s, e) = (cap.node.start_byte(), cap.node.end_byte());
            if starts[row] > s {
                // A capture that starts before its predecessor: captures
                // within one match follow the query's order, not the bytes'.
                // Rare, and correctness beats speed here.
                row = starts.partition_point(|&o| o <= s).saturating_sub(1);
            } else {
                while row + 1 < starts.len() && starts[row + 1] <= s {
                    row += 1;
                }
            }
            let r0 = row;
            let last = e.saturating_sub(1);
            let mut r1 = r0;
            while r1 + 1 < starts.len() && starts[r1 + 1] <= last {
                r1 += 1;
            }
            for r in r0..=r1.min(bounds.len() - 1) {
                let (lo, hi) = bounds[r];
                let s_l = s.max(lo).min(hi);
                let e_l = e.min(hi).max(s_l);
                if s_l >= e_l {
                    continue;
                }
                let cs = char_col(src, lo, s_l, ascii[r]);
                let ce = char_col(src, lo, e_l, ascii[r]);
                if ce > cs {
                    f(r + row_base, cs..ce, name);
                }
            }
        }
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Stream: an append-only document, incrementally parsed
// ---------------------------------------------------------------------------
//
// `dead_code` is allowed item by item across this section on purpose. It is
// the public API of the *library* target (`rano::syntax::Stream`, lib.rs) for
// other crates to use — the editor binary compiles this module privately and
// has no use for the engine yet, so the lint would otherwise fire for every
// type, method and helper here.

/// A node of a [`Stream`]'s tree, as plain data.
///
/// The engine's public surface holds no `tree-sitter` types on purpose: a
/// consumer walks this to build its own layout model, the same way
/// [`Highlighter::classes`] hands back capture *names* rather than nodes.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Node {
    /// A stable identity for this node within its tree, distinct from every other node
    /// in it — tree-sitter's node id.
    ///
    /// What a consumer comparing *positions* cannot do: two nodes can share a byte range
    /// (a zero-width `MISSING` node and the node it was inserted beside) and a parent and
    /// child can share a `start`, so "is this the same node I saw a moment ago" needs a
    /// identity rather than a coordinate. Found missing the day a shell reader asked
    /// whether a child *was* the body it had already found (2026-09-20).
    pub id: usize,
    /// Grammar node kind: `"atx_heading"`, `"fenced_code_block"`,
    /// `"strong_emphasis"`, …
    pub kind: String,
    /// Byte offsets into [`Stream::src`].
    pub start: usize,
    pub end: usize,
    /// The **field** this node was found under in its parent, when it has one:
    /// `"name"` for a definition's identifier, `"body"` for its block, `"type"` for a
    /// declarator's type. `None` for a node reached positionally, and for the root.
    ///
    /// Tree-sitter hands a field out through a cursor rather than storing it on the
    /// node, so a consumer walking [`Self::children`] cannot ask for one — and asking by
    /// *field* is how a grammar-dependent question ("what is this declaration's name")
    /// stays grammar-dependent instead of being re-derived by matching child kinds in
    /// the order they happen to appear. Found missing the day an outline and a shell
    /// reader tried to move off their own parsers (2026-09-20).
    pub field: Option<String>,
    /// Position of `start`, zero-based, **column in bytes**.
    pub start_point: Point,
    /// Position of `end`.
    pub end_point: Point,
    pub has_error: bool,
    pub is_missing: bool,
    /// Tree-sitter's `is_named()`: false for anonymous tokens (punctuation,
    /// keywords like `fn` or `|`), which are in `children` too. A consumer
    /// doing a "split this node's range around its named children" walk —
    /// markdown's block/inline handoff, for one — needs to tell them apart.
    pub named: bool,
    pub children: Vec<Node>,
}

/// One captured range within one line, as plain data — the unit a renderer paints.
///
/// Columns are **characters**, half-open, and within `row`. Later spans may overlap
/// earlier ones: the order is the query's, and a caller deciding colours applies them
/// in it.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// 0-based line, counted by `\n`.
    pub row: usize,
    /// First character of the capture within the row.
    pub start: usize,
    /// One past the last character.
    pub end: usize,
    /// The capture name as the query writes it (`"keyword"`, `"function"`, …), which
    /// the caller maps to a colour — rano's own [`theme`] is only one such mapping.
    pub name: String,
}

/// One query capture, as plain data.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The capture name as the query writes it (`"keyword"`, `"string"`, …).
    pub name: String,
    /// Byte offsets into [`Stream::src`].
    pub start: usize,
    pub end: usize,
}

/// An append-only document, incrementally parsed.
///
/// The editor's [`Highlighter`] full-reparses on every edit, deliberately:
/// reusing a tree across an edit that *shortens* a line leaks byte offsets
/// from the old source into the new tree (see [`Highlighter::refresh`]).
/// This type is the other half of that trade. It only ever grows, and a
/// pure-append edit cannot shorten a line, so handing the previous tree
/// back to the parser is both sound and cheap: the unchanged prefix is
/// reused and the per-push cost stays flat as the document grows. Feeding
/// text through [`Highlighter::refresh`] instead would re-parse the whole
/// document per push — O(N²) over a stream.
///
/// The use is "push, then read": hold one `Stream` per growing document,
/// [`push`](Self::push) each delta as it arrives, and read
/// [`root`](Self::root) (and/or [`captures`](Self::captures)) after each
/// push. For a second grammar over selected parts of the document —
/// markdown's block + inline split, HTML embedding JavaScript, C embedding
/// asm — hold another `Stream` and give it the byte ranges to parse with
/// [`set_included_ranges`](Self::set_included_ranges).
///
/// The engine is generic over [`Lang`]: no grammar-specific knowledge
/// lives here.
///
/// # What the reuse is worth, measured
///
/// How much tree-sitter can reuse is the grammar's decision, not this
/// crate's. Measured on this machine (release build, appending a token or two
/// at a time, 1,000 pushes):
///
/// | grammar | a late push | a one-shot parse of the same text |
/// |---|---|---|
/// | Rust (88 KB) | ~275 µs | ~8.2 ms |
/// | Markdown block (103 KB) | ~9.5 ms | ~12.6 ms |
///
/// Rust reuses nearly everything: a push costs about 3% of a full parse.
/// Markdown's block grammar calls its external scanner in almost every block
/// state, and tree-sitter refuses to reuse a token when the *current* state
/// admits external tokens (`ts_parser__can_reuse_first_leaf`'s
/// `external_lex_state == 0` condition, `parser.c`) — so a markdown push
/// re-lexes essentially the whole document: ~100 ns per document byte, about
/// what a full parse costs. `Tree::edit` is ~0.3 µs and never the cost.
///
/// Either way the per-push cost is linear in the document, so pushing N times
/// costs O(N²) in total. This engine is a constant-factor win over reparsing,
/// not an asymptotic one, and it is not a licence to stream a 10 MB document
/// one token at a time. A consumer that needs better has to keep the settled
/// prefix out of the parser's way itself (parse only the growing tail with
/// [`set_included_ranges`](Self::set_included_ranges) and splice).
///
/// The measurement lives in this module's tests: `an_incremental_push_beats_a_full_reparse`
/// runs by default, `per_push_cost_stays_flat` (ignored, slow) states the
/// flat-cost criterion and fails on it today.
#[allow(dead_code)]
pub struct Stream {
    lang: Lang,
    parser: Parser,
    /// The current parse. `None` before the first push, on an inert stream,
    /// and after a parse that produced nothing (tree-sitter only gives up
    /// on a timeout or a cancellation flag, neither of which is set here).
    tree: Option<Tree>,
    src: String,
    /// Byte ranges the next parse is restricted to (empty = whole document,
    /// tree-sitter's convention).
    ranges: Vec<(usize, usize)>,
    /// `ranges` changed since the parser last saw them.
    ranges_dirty: bool,
    query: Option<Query>,
    /// `(language, query text)` the cached query was compiled from.
    query_key: Option<(Lang, String)>,
    parse_calls: u64,
    /// End of `src` as a `Point`, kept incrementally: two integer updates
    /// per push instead of a rescan of the whole document.
    end_point: TsPoint,
    /// False when `set_language` failed, which makes the stream inert:
    /// `push` is a no-op and `root()` is `None`. Every `Lang` has a valid
    /// grammar, so this is defensive — the same "empty on failure"
    /// convention [`Highlighter::classes`] uses.
    live: bool,
}

#[allow(dead_code)]
impl Stream {
    /// A stream for `lang`, with no text yet.
    ///
    /// Infallible: every `Lang` has a valid grammar. If `set_language` ever
    /// failed the stream would be inert — `push` a no-op, `root()` `None`.
    pub fn new(lang: Lang) -> Self {
        let mut parser = Parser::new();
        let live = parser.set_language(&lang.language()).is_ok();
        Self {
            lang,
            parser,
            tree: None,
            src: String::new(),
            ranges: Vec::new(),
            // Force one `set_included_ranges` before the first parse, so the
            // parser is known to be in the "whole document" state.
            ranges_dirty: true,
            query: None,
            query_key: None,
            parse_calls: 0,
            end_point: TsPoint { row: 0, column: 0 },
            live,
        }
    }

    /// Append `delta` and re-parse.
    ///
    /// With a previous tree the tree is edited for the pure-append range and
    /// handed to the parser, so the unchanged prefix is reused; without one,
    /// a fresh parse. An empty `delta` on a stream that already has a tree
    /// for this source is a no-op. On an inert stream this does nothing at
    /// all.
    pub fn push(&mut self, delta: &str) {
        if !self.live || (delta.is_empty() && self.tree.is_some()) {
            return;
        }
        let old_len = self.src.len();
        let old_end = self.end_point;
        self.src.push_str(delta);
        for b in delta.as_bytes() {
            if *b == b'\n' {
                self.end_point.row += 1;
                self.end_point.column = 0;
            } else {
                self.end_point.column += 1;
            }
        }
        let new_len = self.src.len();

        // The append is zero-width at the old end: the parser learns the
        // source grew and where, and re-uses every subtree that does not
        // touch the tail. `old_end` is the point before the append, so the
        // edit is `start == old_end == new_start` in both bytes and points.
        let edit = InputEdit {
            start_byte: old_len,
            old_end_byte: old_len,
            new_end_byte: new_len,
            start_position: old_end,
            old_end_position: old_end,
            new_end_position: self.end_point,
        };
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(&edit);
        }
        if self.ranges_dirty {
            self.apply_ranges();
        }
        self.parse_calls += 1;
        let parsed = self.parser.parse(self.src.as_bytes(), self.tree.as_ref());
        // A `None` here means the parse produced nothing; drop the tree
        // rather than hand back one that no longer matches `src`, and let
        // the next push do a fresh full parse.
        self.tree = parsed;
    }

    /// The text pushed so far.
    pub fn src(&self) -> &str {
        &self.src
    }

    /// The current tree as this crate's own type, or `None` before the first
    /// push, on an inert stream, and after a parse that produced nothing.
    ///
    /// Built fresh per call: the consumer reads it once per push and the
    /// trees are small (a 200 KB markdown document is a few thousand nodes).
    pub fn root(&self) -> Option<Node> {
        Some(node_of(self.tree.as_ref()?.root_node()))
    }

    /// Restrict the next parse to these byte ranges — tree-sitter's
    /// `set_included_ranges`, and the generic form of "a second grammar over
    /// selected parts of the document". An empty slice means the whole
    /// document.
    ///
    /// The ranges are sorted and merged here rather than trusting the
    /// caller, which is what tree-sitter requires (ordered and
    /// non-overlapping); inverted and empty ranges are dropped. They are
    /// held until the next [`push`](Self::push), which applies them to the
    /// parser before parsing — so text pushed after this call is covered by
    /// the ranges too.
    pub fn set_included_ranges(&mut self, ranges: &[(usize, usize)]) {
        let mut sorted: Vec<(usize, usize)> =
            ranges.iter().copied().filter(|(s, e)| s < e).collect();
        sorted.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(sorted.len());
        for (s, e) in sorted {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.ranges = merged;
        self.ranges_dirty = true;
    }

    /// Run `query` over the current tree: one [`Capture`] per capture, in
    /// document order.
    ///
    /// The query compiles once per `(language, text)` and is cached, the way
    /// [`Highlighter`] caches per language. Empty on an inert stream, before
    /// the first push, and for a query that fails to compile.
    pub fn captures(&mut self, query: &str) -> Vec<Capture> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let stale = self
            .query_key
            .as_ref()
            .is_none_or(|(l, q)| *l != self.lang || q != query);
        if stale {
            match Query::new(&self.lang.language(), query) {
                Ok(q) => {
                    self.query = Some(q);
                    self.query_key = Some((self.lang, query.to_string()));
                }
                Err(_) => {
                    self.query = None;
                    self.query_key = None;
                    return Vec::new();
                }
            }
        }
        let Some(q) = self.query.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let names = q.capture_names();
        let mut cursor = QueryCursor::new();
        let mut caps = cursor.captures(q, tree.root_node(), self.src.as_bytes());
        while let Some((m, i)) = caps.next() {
            let cap = m.captures()[*i];
            let name = names.get(cap.index as usize).copied().unwrap_or("");
            out.push(Capture {
                name: name.to_string(),
                start: cap.node.start_byte(),
                end: cap.node.end_byte(),
            });
        }
        out
    }

    /// The capture spans of the current text as **flat data**: one [`Span`] per
    /// captured range, in the order the query found them.
    ///
    /// This is the highlighting answer for a **renderer**: it wants runs to paint, not
    /// a per-character grid to interrogate, and the difference is not cosmetic. A grid
    /// is one `Vec` per line, so its cost grows with the line count whatever the caller
    /// does with it — measured 2026-09-20 at ~1.9 µs per line — while spans are one
    /// allocation for the whole text and are cheap on long files. An editor that needs
    /// per-character styles at a cursor wants [`Highlighter::refresh`] instead; an
    /// embedder painting a fence or a diff wants these.
    ///
    /// The query is the language's own ([`Lang::query`]), compiled once per stream, so
    /// an embedder never handles query text — the point of the hub.
    ///
    /// Rows and columns are **characters**, not bytes: `start`/`end` are a half-open
    /// char range within [`Span::row`], because that is what a terminal paints in.
    /// Later captures are not merged with earlier ones — a caller deciding colours
    /// applies them in order, which is the rule [`Highlighter::classes`] has always
    /// had.
    ///
    /// Empty before the first push, on an inert stream, and for a language whose query
    /// fails to compile.
    ///
    /// # Cost
    ///
    /// One walk over the tree per call, so O(text) in the number of query matches;
    /// the parse itself is incremental ([`Self::push`]). A caller pushing one token at
    /// a time into a long block therefore does O(text) work per push, and the fix for
    /// that is the window discipline the conversation uses, not a faster walk.
    pub fn spans(&mut self) -> Vec<Span> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        // Process-wide, because the compilation is 8.7 ms and the caller is usually
        // holding one of many blocks — see [`highlight_query`].
        let Some(query) = highlight_query(self.lang) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        Highlighter::for_each_capture(&self.src, tree, &query, 0, |row, range, name| {
            out.push(Span {
                row,
                start: range.start,
                end: range.end,
                name: name.to_string(),
            });
        });
        out
    }

    /// How many `parse` calls this stream has made: one per
    /// [`push`](Self::push) that did any work.
    ///
    /// There is deliberately no "bytes re-parsed" counter — tree-sitter does
    /// not report how much of the prefix it reused, and any proxy for it
    /// would answer the wrong question. Whether the reuse is keeping the
    /// cost flat is a question about time, and the test that asks it
    /// measures it.
    pub fn parse_calls(&self) -> u64 {
        self.parse_calls
    }

    /// Hand the held ranges to the parser. Empty means the whole document.
    fn apply_ranges(&mut self) {
        let len = self.src.len();
        let ranges: Vec<Range> = self
            .ranges
            .iter()
            .map(|&(s, e)| {
                let (s, e) = (s.min(len), e.min(len));
                Range {
                    start_byte: s,
                    end_byte: e,
                    start_point: point_of(&self.src, s).into(),
                    end_point: point_of(&self.src, e).into(),
                }
            })
            .filter(|r| r.start_byte < r.end_byte)
            .collect();
        // Ignoring the result is safe: the ranges were sorted and merged
        // above, so the parser accepts them or they were empty.
        let _ = self.parser.set_included_ranges(&ranges);
        self.ranges_dirty = false;
    }
}

/// **Equality is a node's value, not its identity.** Two parses of the same text are
/// the same tree, and [`Node::id`] differs between them because it is tree-sitter's
/// per-tree node pointer — so deriving this would make `parse(a) == parse(a)` false and
/// take the streaming-equals-one-push property with it. `id` is there for a consumer
/// asking "is this the node I held a moment ago"; that is a different question from
/// "is this the same tree", and only the second belongs in `==`.
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        // `children` compares recursively, so `id` is the only field left out.
        self.kind == other.kind
            && self.start == other.start
            && self.end == other.end
            && self.field == other.field
            && self.start_point == other.start_point
            && self.end_point == other.end_point
            && self.has_error == other.has_error
            && self.is_missing == other.is_missing
            && self.named == other.named
            && self.children == other.children
    }
}

impl Eq for Node {}

/// Deep-copy a `tree-sitter` node into this crate's own [`Node`].
#[allow(dead_code)]
fn node_of(n: tree_sitter::Node<'_>) -> Node {
    Node {
        id: n.id(),
        kind: n.kind().to_string(),
        start: n.start_byte(),
        end: n.end_byte(),
        // The root has no parent, so it is under no field.
        field: None,
        start_point: n.start_position().into(),
        end_point: n.end_position().into(),
        has_error: n.has_error(),
        is_missing: n.is_missing(),
        named: n.is_named(),
        children: children_of(n),
    }
}

/// A node's children, each labelled with the field it appears under.
///
/// Through a cursor rather than `children()`, because the cursor is the only thing that
/// knows a child's field name.
fn children_of(n: tree_sitter::Node<'_>) -> Vec<Node> {
    // Navigation rather than `n.children(&mut cursor)`: the cursor is the only thing that
    // knows a child's field name, and holding it mutably for the length of a `children()`
    // iterator is what makes asking for the name a borrow error.
    let mut cursor = n.walk();
    let mut pairs: Vec<(Option<String>, tree_sitter::Node<'_>)> = Vec::new();
    if cursor.goto_first_child() {
        loop {
            pairs.push((cursor.field_name().map(str::to_string), cursor.node()));
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    pairs
        .into_iter()
        .map(|(field, child)| {
            let mut node = node_of(child);
            node.field = field;
            node
        })
        .collect()
}

/// A position in a document: line and column, both zero-based, the column in **bytes**.
///
/// Rano's own rather than tree-sitter's, for the reason the rest of the public API is:
/// a consumer of [`Node`] should not have to depend on a parser to read a coordinate.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: usize,
    pub column: usize,
}

impl From<TsPoint> for Point {
    fn from(p: TsPoint) -> Point {
        Point {
            row: p.row,
            column: p.column,
        }
    }
}

impl From<Point> for TsPoint {
    fn from(p: Point) -> TsPoint {
        TsPoint {
            row: p.row,
            column: p.column,
        }
    }
}

/// `(row, column)` — both zero-based, the column in BYTES — of byte offset
/// `off` in `src`, clamped into `src` and onto a char boundary.
#[allow(dead_code)]
fn point_of(src: &str, off: usize) -> Point {
    let mut off = off.min(src.len());
    while !src.is_char_boundary(off) {
        off -= 1;
    }
    let upto = &src[..off];
    match upto.rfind('\n') {
        Some(nl) => Point {
            row: upto.bytes().filter(|b| *b == b'\n').count(),
            column: off - nl - 1,
        },
        None => Point {
            row: 0,
            column: off,
        },
    }
}

/// tree-sitter reports byte offsets within a row; diagnostics use char
/// columns, so count the chars that make up the byte prefix.
fn char_pos(lines: &[Vec<char>], p: tree_sitter::Point) -> (usize, usize) {
    let Some(line) = lines.get(p.row) else {
        return (p.row, 0);
    };
    let mut bytes = 0usize;
    let mut col = 0usize;
    for ch in line {
        if bytes >= p.column {
            break;
        }
        bytes += ch.len_utf8();
        col += 1;
    }
    (p.row, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn buf_named(name: &str, text: &str) -> Buffer {
        let mut b = Buffer::new();
        b.lines = text.lines().map(|l| l.chars().collect()).collect();
        b.name = Some(PathBuf::from(name));
        b
    }

    fn style_at(hl: &Highlighter, row: usize, col: usize) -> Option<Style> {
        hl.style_at(Pos { row, col })
    }

    #[test]
    fn classes_name_the_tokens_of_raw_text_without_a_buffer() {
        let src = "let done = build(); // tail\n";
        let mut hl = Highlighter::new();
        let grid = hl.classes(src, Lang::Rust);
        assert_eq!(
            grid.len(),
            2,
            "one row per \\n-split line, trailing piece included"
        );
        let row = &grid[0];
        assert_eq!(row.len(), src.lines().next().unwrap().len());
        // `let` is a keyword, `build` a function call, the comment a comment,
        // the brackets punctuation. A plain identifier is captured by nothing
        // in the Rust query and stays `None` — the embedder reads that as
        // "plain text", which is what it is.
        let class = |needle: &str| {
            let at = src.find(needle).unwrap();
            row[at].clone()
        };
        assert_eq!(class("let").as_deref(), Some("keyword"));
        assert_eq!(class("build").as_deref(), Some("function"));
        assert_eq!(class("// tail").as_deref(), Some("comment"));
        assert_eq!(class("(").as_deref(), Some("punctuation.bracket"));
        assert_eq!(class("done"), None);
        assert!(grid[1].iter().all(|c| c.is_none()), "empty tail row");
    }

    #[test]
    fn classes_of_empty_text_is_one_empty_row_not_a_panic() {
        let mut hl = Highlighter::new();
        let grid = hl.classes("", Lang::Rust);
        assert_eq!(grid.len(), 1, "'' splits to one empty line");
        assert!(grid[0].is_empty());
    }

    /// **A multi-byte character shifts nothing but its own cell.**
    ///
    /// The em-dash is three bytes and one char, and two places in
    /// `for_each_capture` counted the one where the other was meant:
    /// `line_offsets` advanced by the char count, so every line after a
    /// multi-byte line started bytes early in tree-sitter's coordinates and
    /// its classes landed cells to the right; and the per-line clamp used
    /// the char count as the byte length, so a capture ending near the end
    /// of the multi-byte line itself lost its last cells. Seen from the
    /// head that consumes this grid as `tail o[0mff` — a reset sequence
    /// landing mid-word, because a class run ended two columns before the
    /// token did.
    #[test]
    fn a_multibyte_character_shifts_nothing_but_its_own_cell() {
        let src = "let s = \"a—b\"; // dash\nlet done = build(); // tail\n";
        // Char column of the first char of `needle` — `find` answers bytes,
        // and the grid is one cell per char.
        let col_of = |line: &str, needle: &str| {
            let b = line.find(needle).unwrap();
            line[..b].chars().count()
        };
        let mut hl = Highlighter::new();
        let grid = hl.classes(src, Lang::Rust);
        assert_eq!(grid.len(), 3, "one row per \\n-split line, tail included");

        // The line WITH the dash. The clamp bug lived here: the string
        // capture's byte end was clamped to the char count, so the closing
        // quote fell out of the capture and rendered plain.
        let l0 = src.split('\n').next().unwrap();
        assert_eq!(grid[0][col_of(l0, "let")].as_deref(), Some("keyword"));
        assert_eq!(
            grid[0][col_of(l0, "—")].as_deref(),
            Some("string"),
            "the dash itself is inside the string literal"
        );
        let close = col_of(l0, "b\"") + 1;
        assert_eq!(
            grid[0][close].as_deref(),
            Some("string"),
            "the closing quote is still inside the capture"
        );
        assert_eq!(grid[0][col_of(l0, "// dash")].as_deref(), Some("comment"));

        // The line AFTER it. The line-offset bug lived here: this line's
        // byte start was short by the dash's two extra bytes, so every
        // class landed two cells to the right.
        let l1 = src.split('\n').nth(1).unwrap();
        assert_eq!(grid[1][col_of(l1, "let")].as_deref(), Some("keyword"));
        assert_eq!(grid[1][col_of(l1, "build")].as_deref(), Some("function"));
        assert_eq!(grid[1][col_of(l1, "// tail")].as_deref(), Some("comment"));
        assert!(grid[2].iter().all(|c| c.is_none()), "empty tail row");
    }

    #[test]
    fn detects_languages() {
        assert_eq!(detect(Some(Path::new("a/b.rs")), None), Some(Lang::Rust));
        assert_eq!(detect(Some(Path::new("main.go")), None), Some(Lang::Go));
        assert_eq!(detect(Some(Path::new("run.SH")), None), Some(Lang::Bash));
        assert_eq!(detect(Some(Path::new("x.py")), None), Some(Lang::Python));
        assert_eq!(detect(Some(Path::new("x.pyw")), None), Some(Lang::Python));
        assert_eq!(detect(Some(Path::new("x.c")), None), Some(Lang::C));
        assert_eq!(detect(Some(Path::new("x.h")), None), Some(Lang::C));
        assert_eq!(detect(Some(Path::new("x.json")), None), Some(Lang::Json));
        assert_eq!(
            detect(Some(Path::new("x.lisp")), None),
            Some(Lang::CommonLisp)
        );
        assert_eq!(
            detect(Some(Path::new("x.cl")), None),
            Some(Lang::CommonLisp)
        );
        assert_eq!(
            detect(Some(Path::new("x.lsp")), None),
            Some(Lang::CommonLisp)
        );
        assert_eq!(
            detect(Some(Path::new("sys.asd")), None),
            Some(Lang::CommonLisp)
        );
        assert_eq!(
            detect(Some(Path::new("a.js")), None),
            Some(Lang::JavaScript)
        );
        assert_eq!(
            detect(Some(Path::new("a.mjs")), None),
            Some(Lang::JavaScript)
        );
        assert_eq!(
            detect(Some(Path::new("a.ts")), None),
            Some(Lang::TypeScript)
        );
        assert_eq!(detect(Some(Path::new("a.tsx")), None), Some(Lang::Tsx));
        assert_eq!(detect(Some(Path::new("a.md")), None), Some(Lang::Markdown));
        assert_eq!(detect(Some(Path::new("a.toml")), None), Some(Lang::Toml));
        assert_eq!(detect(Some(Path::new("a.yaml")), None), Some(Lang::Yaml));
        assert_eq!(detect(Some(Path::new("a.yml")), None), Some(Lang::Yaml));
        assert_eq!(detect(Some(Path::new("a.html")), None), Some(Lang::Html));
        assert_eq!(detect(Some(Path::new("a.htm")), None), Some(Lang::Html));
        assert_eq!(detect(Some(Path::new("a.css")), None), Some(Lang::Css));
        assert_eq!(detect(Some(Path::new("a.lua")), None), Some(Lang::Lua));
        assert_eq!(detect(Some(Path::new("a.rb")), None), Some(Lang::Ruby));
        assert_eq!(detect(Some(Path::new("a.php")), None), Some(Lang::Php));
        assert_eq!(detect(Some(Path::new("A.java")), None), Some(Lang::Java));
        assert_eq!(detect(Some(Path::new("build.mk")), None), Some(Lang::Make));
        assert_eq!(
            detect(Some(Path::new("x.dockerfile")), None),
            Some(Lang::Dockerfile)
        );
        assert_eq!(detect(Some(Path::new("a.ini")), None), Some(Lang::Ini));
        assert_eq!(detect(Some(Path::new("a.cfg")), None), Some(Lang::Ini));
        assert_eq!(detect(Some(Path::new("a.diff")), None), Some(Lang::Diff));
        assert_eq!(detect(Some(Path::new("a.patch")), None), Some(Lang::Diff));
        assert_eq!(detect(Some(Path::new("a.el")), None), Some(Lang::Elisp));
        assert_eq!(detect(Some(Path::new("a.scm")), None), Some(Lang::Scheme));
        assert_eq!(detect(Some(Path::new("a.sql")), None), Some(Lang::Sql));
        assert_eq!(detect(Some(Path::new("a.clj")), None), Some(Lang::Clojure));
        assert_eq!(detect(Some(Path::new("a.edn")), None), Some(Lang::Clojure));
        assert_eq!(detect(None, None), None);
    }

    // Files known by name, not extension.
    #[test]
    fn detects_filenames() {
        assert_eq!(detect(Some(Path::new("Makefile")), None), Some(Lang::Make));
        assert_eq!(detect(Some(Path::new("makefile")), None), Some(Lang::Make));
        assert_eq!(
            detect(Some(Path::new("GNUmakefile")), None),
            Some(Lang::Make)
        );
        assert_eq!(
            detect(Some(Path::new("Makefile.dev")), None),
            Some(Lang::Make)
        );
        assert_eq!(
            detect(Some(Path::new("Dockerfile")), None),
            Some(Lang::Dockerfile)
        );
        assert_eq!(
            detect(Some(Path::new("dockerfile.prod")), None),
            Some(Lang::Dockerfile)
        );
        assert_eq!(detect(Some(Path::new(".gitconfig")), None), Some(Lang::Ini));
        assert_eq!(
            detect(Some(Path::new(".editorconfig")), None),
            Some(Lang::Ini)
        );
        // A directory component must not count: only the file name is
        // matched.
        assert_eq!(detect(Some(Path::new("dockerfile/x.txt")), None), None);
    }

    #[test]
    fn detects_shebang_languages() {
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/bin/sh")),
            Some(Lang::Bash)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/env bash")),
            Some(Lang::Bash)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#! /bin/sh")),
            Some(Lang::Bash)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/env python3")),
            Some(Lang::Python)
        );
        assert_eq!(
            detect(
                Some(Path::new("letibot")),
                Some("#!/usr/bin/env -S python3 -u")
            ),
            Some(Lang::Python)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/sbcl --script")),
            Some(Lang::CommonLisp)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/env clisp")),
            Some(Lang::CommonLisp)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/node")),
            Some(Lang::JavaScript)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/env ruby")),
            Some(Lang::Ruby)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/php")),
            Some(Lang::Php)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/lua5.4")),
            Some(Lang::Lua)
        );
        // No grammar for the interpreter, or no shebang at all.
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/perl")),
            None
        );
        assert_eq!(detect(Some(Path::new("letibot")), Some("echo hi")), None);
        // A known extension still wins over the shebang.
        assert_eq!(
            detect(Some(Path::new("x.py")), Some("#!/bin/sh")),
            Some(Lang::Python)
        );
    }

    #[test]
    fn highlights_rust() {
        let b = buf_named(
            "t.rs",
            "fn main() -> i32 {\n    let x = 42; // fourty two\n    x\n}",
        );
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "fn" keyword
        assert!(style_at(&hl, 0, 3).is_some()); // "main" function
        let line1: String = b.lines[1].iter().collect();
        assert!(style_at(&hl, 1, line1.find('4').unwrap()).is_some()); // 42
        assert!(style_at(&hl, 1, line1.find("fourty").unwrap()).is_some()); // comment
        assert!(style_at(&hl, 1, line1.find('x').unwrap()).is_none()); // plain local
    }

    #[test]
    fn highlights_go() {
        let b = buf_named(
            "t.go",
            "package main\n\nfunc Add(a int) int {\n\treturn a + 1\n}",
        );
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "package" keyword
        assert!(style_at(&hl, 2, 5).is_some()); // "Add" function
    }

    #[test]
    fn highlights_python() {
        let b = buf_named("t.py", "def fn(x):\n    return x\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "def" keyword
        assert!(style_at(&hl, 0, 4).is_some()); // "fn" function
    }

    #[test]
    fn highlights_c() {
        let b = buf_named("t.c", "int main(void) {\n    return 0;\n}\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "int" type
        assert!(style_at(&hl, 0, 4).is_some()); // "main" function
    }

    #[test]
    fn highlights_json() {
        let b = buf_named("t.json", "{\"k\": 1}\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 1).is_some()); // "k" property
        assert!(style_at(&hl, 0, 6).is_some()); // 1 number
    }

    #[test]
    fn highlights_commonlisp() {
        let b = buf_named(
            "t.lisp",
            ";; greet\n(defun greet (name)\n  (format t \"Hello, ~a!\" name))\n",
        );
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // comment
        assert!(style_at(&hl, 1, 1).is_some()); // defun keyword
        assert!(style_at(&hl, 1, 7).is_some()); // greet, the defined function
        assert!(style_at(&hl, 1, 14).is_none()); // plain parameter
        let line2: String = b.lines[2].iter().collect();
        assert!(style_at(&hl, 2, line2.find("format").unwrap()).is_some()); // call head
        assert!(style_at(&hl, 2, line2.find(" t ").unwrap() + 1).is_some()); // t constant
        assert!(style_at(&hl, 2, line2.find('~').unwrap()).is_some()); // ~a directive
        assert!(style_at(&hl, 2, line2.find("name").unwrap()).is_none()); // plain argument
    }

    // The query must speak the capture vocabulary `theme` maps: a name it
    // does not know renders uncoloured.
    #[test]
    fn commonlisp_classes_use_the_shared_vocabulary() {
        let src = "(defun greet (x) :hello)\n";
        let mut hl = Highlighter::new();
        let grid = hl.classes(src, Lang::CommonLisp);
        let row = &grid[0];
        assert_eq!(row[src.find("defun").unwrap()].as_deref(), Some("keyword"));
        assert_eq!(row[src.find("greet").unwrap()].as_deref(), Some("function"));
        assert_eq!(
            row[src.find(":hello").unwrap()].as_deref(),
            Some("constant")
        );
        // Plain symbols are captured as "variable" for embedders; `theme`
        // has no entry for it, so the editor renders them uncoloured (see
        // highlights_commonlisp).
        assert_eq!(
            row[src.find("(x)").unwrap() + 1].as_deref(),
            Some("variable")
        );
    }

    #[test]
    fn highlights_javascript() {
        let b = buf_named("t.js", "const x = 1;\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // const keyword
        assert!(style_at(&hl, 0, 10).is_some()); // 1 number
    }

    #[test]
    fn highlights_typescript() {
        let b = buf_named("t.ts", "const x: number = 1;\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // const keyword
        assert!(style_at(&hl, 0, 9).is_some()); // number builtin type
    }

    // TSX shares the TypeScript query; the grammar is the TSX one.
    #[test]
    fn highlights_tsx() {
        let b = buf_named("t.tsx", "const x: number = 1;\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // const keyword
        assert!(style_at(&hl, 0, 9).is_some()); // number builtin type
    }

    // The markdown query is rano's own; assert the capture names, not just
    // that something is coloured. Code-fence content is coloured as one
    // literal run — nano colours the whole fence too — and it is NOT
    // re-parsed: no injection grammar runs, so a ```rust block is cyan, not
    // Rust-coloured.
    #[test]
    fn markdown_classes_name_the_block_structure() {
        let src = "# Title\n\n- item\n\n```rust\nfn main() {}\n```\n";
        let mut hl = Highlighter::new();
        let grid = hl.classes(src, Lang::Markdown);
        let row0 = &grid[0];
        assert_eq!(row0[0].as_deref(), Some("keyword")); // `#`
        assert_eq!(row0[2].as_deref(), Some("type")); // Title
        assert_eq!(grid[2][0].as_deref(), Some("punctuation.bracket")); // `-`
        assert_eq!(grid[4][0].as_deref(), Some("punctuation.bracket")); // fence
        assert_eq!(grid[4][3].as_deref(), Some("attribute")); // rust info string
        assert_eq!(grid[5][0].as_deref(), Some("text.literal")); // fence content
        // The whole body, not just its first character: `fn` is not a keyword
        // here, because the fence is not parsed as Rust.
        assert_eq!(grid[5][3].as_deref(), Some("text.literal"));
        assert_eq!(grid[5][0], grid[5][3]);
    }

    #[test]
    fn highlights_toml() {
        let b = buf_named("t.toml", "key = \"v\"\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // key property
        assert!(style_at(&hl, 0, 7).is_some()); // "v" string
    }

    #[test]
    fn highlights_yaml() {
        let b = buf_named("t.yaml", "key: value # note\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // key property
        assert!(style_at(&hl, 0, 11).is_some()); // comment
    }

    #[test]
    fn highlights_html() {
        let b = buf_named("t.html", "<div class=\"x\">t</div>\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 1).is_some()); // div tag
        assert!(style_at(&hl, 0, 5).is_some()); // class attribute
    }

    #[test]
    fn highlights_css() {
        let b = buf_named("t.css", "a {\n  color: red;\n}\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // a tag selector
        assert!(style_at(&hl, 1, 2).is_some()); // color property
    }

    #[test]
    fn highlights_lua() {
        let b = buf_named("t.lua", "local x = 1\nfunction f() end\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // local keyword
        assert!(style_at(&hl, 1, 9).is_some()); // f function
    }

    #[test]
    fn highlights_ruby() {
        let b = buf_named("t.rb", "def foo\n  1\nend\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // def keyword
        assert!(style_at(&hl, 0, 4).is_some()); // foo method
    }

    #[test]
    fn highlights_php() {
        let b = buf_named("t.php", "<?php\nfunction f() {}\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 1, 0).is_some()); // function keyword
        assert!(style_at(&hl, 1, 9).is_some()); // f function
    }

    #[test]
    fn highlights_java() {
        let b = buf_named("A.java", "class A {\n  void m() {}\n}\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // class keyword
        assert!(style_at(&hl, 1, 2).is_some()); // void type
        assert!(style_at(&hl, 1, 7).is_some()); // m method
    }

    #[test]
    fn highlights_make() {
        let b = buf_named("Makefile", "all:\n\techo hi\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // all target
        assert!(style_at(&hl, 0, 3).is_some()); // : rule delimiter
    }

    #[test]
    fn highlights_dockerfile() {
        let b = buf_named("Dockerfile", "FROM alpine:3\nRUN echo hi\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // FROM keyword
        assert!(style_at(&hl, 1, 0).is_some()); // RUN keyword
    }

    #[test]
    fn highlights_ini() {
        let b = buf_named("t.ini", "[sec]\nk = v\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 1).is_some()); // sec section
        assert!(style_at(&hl, 1, 0).is_some()); // k setting name
    }

    #[test]
    fn highlights_diff() {
        let b = buf_named("t.diff", "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // old_file
        assert!(style_at(&hl, 1, 0).is_some()); // new_file
        assert!(style_at(&hl, 2, 0).is_some()); // @@ hunk location
        assert!(style_at(&hl, 3, 0).is_some()); // -old deletion
        assert!(style_at(&hl, 4, 0).is_some()); // +new addition
    }

    #[test]
    fn highlights_elisp() {
        let b = buf_named("t.el", "(defun foo (x) x)\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 1).is_some()); // defun keyword
        assert!(style_at(&hl, 0, 7).is_some()); // foo function
    }

    #[test]
    fn highlights_scheme() {
        let b = buf_named("t.scm", "(define (f x) x)\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 1).is_some()); // define keyword
        assert!(style_at(&hl, 0, 9).is_some()); // f function
    }

    #[test]
    fn highlights_sql() {
        let b = buf_named("t.sql", "SELECT a FROM t;\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // SELECT keyword
        assert!(style_at(&hl, 0, 9).is_some()); // FROM keyword
    }

    #[test]
    fn highlights_clojure() {
        let b = buf_named("t.clj", "(defn f [x] x)\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 1).is_some()); // defn keyword
        assert!(style_at(&hl, 0, 6).is_some()); // f function
    }

    #[test]
    fn highlights_bash() {
        let b = buf_named("t.sh", "#!/bin/sh\necho \"hello\"\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // comment
        assert!(style_at(&hl, 1, 0).is_some()); // echo builtin
    }

    // Extension-less scripts (e.g. ~/bin/letibot) are detected by shebang.
    #[test]
    fn highlights_extensionless_shebang_script() {
        let b = buf_named("letibot", "#!/usr/bin/env bash\necho \"hello\"\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // shebang comment
        assert!(style_at(&hl, 1, 0).is_some()); // echo builtin
    }

    #[test]
    fn scratch_buffer_has_no_highlighting() {
        let mut b = Buffer::new();
        b.lines = vec!["fn main() {}".chars().collect()];
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(hl.style_at(Pos { row: 0, col: 0 }).is_none());
    }

    #[test]
    fn multiline_comment_spans_lines() {
        let b = buf_named("t.rs", "/// doc\n/// line two\nfn main() {}");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 4).is_some()); // "doc"
        assert!(style_at(&hl, 1, 4).is_some()); // "line"
    }

    // Regression: re-parsing a buffer that just shrank (backspace) used to
    // panic inside tree-sitter — the reused incremental tree still carried
    // byte offsets from the longer source.
    #[test]
    fn refresh_after_shrink_does_not_panic() {
        let mut b = buf_named("t.sh", "#!/usr/bin/env bash\nbogus_var=9\necho \"ok\"\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        // Delete line 1 char-by-char, re-parsing after every keystroke.
        let line = 1;
        while !b.lines[line].is_empty() {
            b.lines[line].pop();
            hl.refresh(&b);
        }
        // Grow it again to exercise the other direction too.
        b.lines[line].extend("x=1".chars());
    }
}

/// Driver tests for [`Stream`], the append-only parse engine.
///
/// The measurement in `per_push_cost_stays_flat` is the number the engine
/// exists for: it prints per-push medians and p95 for the first and last 100
/// pushes and asserts the cost does not grow with the document.
#[cfg(test)]
mod stream_tests {
    use super::*;
    use std::time::Instant;

    // ---------- helpers ----------

    /// Deterministic xorshift, so the chunk boundaries are reproducible and
    /// the suite needs no rng dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        /// A value in `0..n` (0 when `n` is 0).
        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    /// `i` clamped into `doc` and onto a char boundary. Both documents carry
    /// multibyte text, so this is what keeps the random cuts valid — and
    /// makes the pushes exercise the byte-offset side of the incremental
    /// edit (`Point::column` is in bytes).
    fn snap(doc: &str, i: usize) -> usize {
        let mut i = i.min(doc.len());
        while !doc.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// Cut points through `doc` for `n` pieces: char-boundary offsets, with
    /// a deterministic jitter so the pieces are not all the same size.
    fn cut_points(doc: &str, n: usize, rng: &mut Rng) -> Vec<usize> {
        let step = (doc.len() / n.max(1)).max(1);
        let mut cuts = Vec::with_capacity(n + 1);
        let mut at = step;
        while at < doc.len() {
            cuts.push(snap(doc, at));
            at += step + rng.below(step);
        }
        cuts.push(doc.len());
        cuts.dedup();
        cuts
    }

    /// Push `doc` to `s` in the pieces `cuts` delimits, and say how many
    /// pushes that took.
    fn push_in_pieces(s: &mut Stream, doc: &str, cuts: &[usize]) -> usize {
        let mut at = 0;
        let mut n = 0;
        for &cut in cuts {
            if cut <= at {
                continue;
            }
            s.push(&doc[at..cut]);
            at = cut;
            n += 1;
        }
        n
    }

    /// Every node of the tree, the root included.
    fn count_nodes(n: &Node) -> usize {
        1 + n.children.iter().map(count_nodes).sum::<usize>()
    }

    // ---------- documents ----------

    /// A ≥ 2 KB Rust document, with a multibyte char in the header.
    fn rust_doc() -> String {
        let mut s = String::from(
            "//! a module — with an em dash\n\nuse std::fmt;\n\npub struct A { pub x: usize }\n\n",
        );
        for i in 0..30 {
            s.push_str(&format!(
                "/// step {i} → done\npub fn f{i}(x: usize) -> usize {{\n    let y = x * {i} + 1;\n    if y > 100 {{ y / 2 }} else {{ y + {i} }}\n}}\n\n"
            ));
        }
        s.push_str("fn main() {\n    println!(\"{}\", f0(1));\n}\n");
        s
    }

    /// A ≥ 2 KB markdown document with a heading, a paragraph holding
    /// `**bold**`, `` `code` `` and a link, a fenced block, a list and a pipe
    /// table.
    fn md_doc() -> String {
        let mut s = String::from(MD_HEAD);
        for i in 0..24 {
            s.push_str(&format!(
                "Paragraph {i} with **bold {i}** and `code{i}` and a [link](http://x/{i}) — trailing text.\n\n"
            ));
        }
        s
    }

    /// The fixed head every markdown document in this module starts with —
    /// the four constructs the brief's §3.1 names, at stable offsets.
    const MD_HEAD: &str = "# Streaming\n\nA paragraph with **bold** and `code` and a [link](http://x) — here.\n\n```rust\nfn main() {}\n```\n\n- one\n- two\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n## Second\n\n";

    /// A ~100 KB markdown document (the §3.5 workload).
    fn big_md() -> String {
        let mut s = String::with_capacity(110_000);
        for i in 0..1100 {
            s.push_str(&format!(
                "Paragraph {i} with **bold {i}** and `code{i}` plus a [link](http://x/{i}) and trailing text.\n\n"
            ));
        }
        s
    }

    /// A ~100 KB Rust document.
    fn big_rust() -> String {
        let mut s = String::with_capacity(100_000);
        for i in 0..1500 {
            s.push_str(&format!(
                "pub fn f{i}(x: usize) -> usize {{ let y = x * {i}; y + 1 }}\n"
            ));
        }
        s
    }

    // ---------- tree helpers (what a consumer would write) ----------

    /// The inline-content byte ranges of a markdown block tree: every
    /// `inline` and `pipe_table_cell` node's range, split around its NAMED
    /// children — those the block grammar already parsed, so the inline
    /// grammar must not re-parse them. Sorted, which is also what
    /// [`Stream::set_included_ranges`] wants.
    ///
    /// This is the consumer's job, not the engine's: `Stream` is generic and
    /// knows nothing about markdown.
    fn inline_ranges(root: &Node) -> Vec<(usize, usize)> {
        fn walk(n: &Node, out: &mut Vec<(usize, usize)>) {
            if n.kind == "inline" || n.kind == "pipe_table_cell" {
                let mut at = n.start;
                for c in n.children.iter().filter(|c| c.named) {
                    if c.start > at {
                        out.push((at, c.start));
                    }
                    at = at.max(c.end);
                }
                if at < n.end {
                    out.push((at, n.end));
                }
            }
            for c in &n.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(root, &mut out);
        out.sort_unstable();
        out
    }

    /// Every node of `root` below `kind`, depth first.
    fn nodes_of<'a>(n: &'a Node, kind: &str, out: &mut Vec<&'a Node>) {
        if n.kind == kind {
            out.push(n);
        }
        for c in &n.children {
            nodes_of(c, kind, out);
        }
    }

    fn find<'a>(n: &'a Node, kind: &str) -> Vec<&'a Node> {
        let mut out = Vec::new();
        nodes_of(n, kind, &mut out);
        out
    }

    /// Assert every node but the root lies inside one of `ranges` — i.e. the
    /// parse stayed within the text it was given.
    fn assert_inside(root: &Node, ranges: &[(usize, usize)], what: &str) {
        fn check(n: &Node, ranges: &[(usize, usize)], top: bool, what: &str) {
            if !top {
                assert!(
                    ranges.iter().any(|(a, b)| n.start >= *a && n.end <= *b),
                    "{what}: node {:?} [{}..{}] is outside every included range {ranges:?}",
                    n.kind,
                    n.start,
                    n.end
                );
            }
            for c in &n.children {
                check(c, ranges, false, what);
            }
        }
        check(root, ranges, true, what);
    }

    // ---------- §3.1 streaming == one push ----------

    /// The property the consumer's renderer is built on: a document pushed in
    /// random chunks parses to exactly the tree a single push gives.
    fn assert_chunked_equals_whole(lang: Lang, doc: &str, seed: u64) {
        let mut whole = Stream::new(lang);
        whole.push(doc);
        let want = whole.root().expect("one-push tree");
        assert!(
            count_nodes(&want) >= 20,
            "document too small to be useful: {} nodes",
            count_nodes(&want)
        );

        let mut rng = Rng(seed);
        let cuts = cut_points(doc, 7, &mut rng);
        assert!(
            cuts.len() >= 4,
            "expected several chunks, got {}",
            cuts.len()
        );
        let mut chunked = Stream::new(lang);
        let pushes = push_in_pieces(&mut chunked, doc, &cuts);
        assert_eq!(chunked.src(), doc);
        assert_eq!(chunked.root().as_ref(), Some(&want));
        assert_eq!(chunked.parse_calls(), pushes as u64, "one parse per chunk");
        assert_eq!(whole.parse_calls(), 1);
    }

    #[test]
    fn streaming_equals_one_push_for_rust() {
        assert_chunked_equals_whole(Lang::Rust, &rust_doc(), 0x1234_5678_9abc_def0);
    }

    #[test]
    fn streaming_equals_one_push_for_markdown() {
        let doc = md_doc();
        assert!(doc.len() >= 2048, "{} bytes", doc.len());
        assert_chunked_equals_whole(Lang::Markdown, &doc, 0x0fed_cba9_8765_4321);
    }

    // ---------- §3.2 / §3.4 the two passes ----------

    /// Push a markdown document through a block stream and an inline stream
    /// that is fed the block tree's inline ranges on every push, checking the
    /// invariant after each step (not just at the end): the inline parse
    /// never reaches outside the ranges it was given, and the ranges are
    /// sorted and inside the text pushed so far.
    fn run_two_passes(doc: &str, chunks: usize, seed: u64) -> (Stream, Stream) {
        let mut block = Stream::new(Lang::Markdown);
        let mut inline = Stream::new(Lang::MarkdownInline);
        let mut rng = Rng(seed);
        let cuts = cut_points(doc, chunks, &mut rng);
        let mut at = 0;
        let mut pushed_ranges: Vec<(usize, usize)> = Vec::new();
        let mut steps = 0;
        for &cut in &cuts {
            if cut <= at {
                continue;
            }
            let delta = &doc[at..cut];
            block.push(delta);

            // What the previous inline parse produced must sit inside the
            // ranges it was given, every step, all the way through.
            if let Some(tree) = inline.root() {
                assert_inside(&tree, &pushed_ranges, "inline tree between pushes");
            }

            let ranges = inline_ranges(&block.root().expect("block tree"));
            for w in ranges.windows(2) {
                assert!(w[0].1 <= w[1].0, "ranges not sorted/disjoint: {ranges:?}");
            }
            for (a, b) in &ranges {
                assert!(a <= b && *b <= delta.len() + at, "{a}..{b} past {cut}");
            }
            inline.set_included_ranges(&ranges);
            inline.push(delta);
            pushed_ranges = ranges;
            at = cut;
            steps += 1;
        }
        assert!(steps >= 2, "the document must really be chunked");
        (block, inline)
    }

    #[test]
    fn two_passes_name_the_inline_constructs() {
        let doc = "# Title\n\nA para with **bold** and `code` here [link](http://x).\n\n| a | b |\n| - | - |\n| 1 | 2 |\n";
        let (block, inline) = run_two_passes(doc, 5, 0xabcd_1234_5678_9f01);

        // The inline stream's ranges come from the block tree, never from a
        // path or a language guess.
        let ranges = inline_ranges(&block.root().unwrap());
        assert!(!ranges.is_empty());

        // Its tree covers exactly the first range's start to the last
        // range's end — nothing was parsed from outside them.
        let root = inline.root().expect("inline tree");
        assert_eq!(root.start, ranges.first().unwrap().0);
        assert_eq!(root.end, ranges.last().unwrap().1);
        assert_inside(&root, &ranges, "final inline tree");

        // The inline grammar's own names for the constructs: `strong_emphasis`
        // (`**bold**`), `code_span` (`` `code` ``) and `inline_link`.
        for (kind, want) in [
            ("strong_emphasis", "**bold**"),
            ("code_span", "`code`"),
            ("inline_link", "[link](http://x)"),
        ] {
            let found = find(&root, kind);
            let node = found
                .first()
                .unwrap_or_else(|| panic!("no {kind} in the inline tree"));
            assert_eq!(
                &doc[node.start..node.end],
                want,
                "{kind} at {}..{}",
                node.start,
                node.end
            );
        }

        // The inline stream runs its own grammar: the block kinds are absent.
        assert!(find(&root, "fenced_code_block").is_empty());
        assert!(find(&root, "paragraph").is_empty());

        // A second pass re-reads the same stream: the query cache path is
        // exercised separately below; here the tree must simply be stable.
        let again = inline.root().unwrap();
        assert_eq!(again, root);
    }

    #[test]
    fn inline_stream_stays_correct_while_ranges_grow() {
        // §3.4: the range list changes at the tail on every push while the
        // earlier ranges are untouched. `run_two_passes` asserts the
        // invariant after every step; this pins the final state too.
        let doc = md_doc();
        let (block, inline) = run_two_passes(&doc, 12, 0x5555_aaaa_3333_cccc);
        let ranges = inline_ranges(&block.root().unwrap());
        let root = inline.root().expect("inline tree");
        assert_inside(&root, &ranges, "final inline tree");
        assert_eq!(root.start, ranges.first().unwrap().0);
        assert_eq!(root.end, ranges.last().unwrap().1);
        // The head's constructs are still found at their real offsets.
        for (kind, want) in [("strong_emphasis", "**bold**"), ("code_span", "`code`")] {
            let node = find(&root, kind)
                .first()
                .copied()
                .map(|n| &doc[n.start..n.end]);
            assert_eq!(node, Some(want), "{kind}");
        }
    }

    #[test]
    fn included_ranges_are_sorted_merged_and_clamped() {
        let doc = "aaa bbb ccc ddd\n";
        let mut s = Stream::new(Lang::MarkdownInline);
        // Deliberately unsorted, overlapping, inverted and past the end.
        s.set_included_ranges(&[(8, 12), (0, 3), (10, 40), (5, 5), (4, 2)]);
        s.push(doc);
        let root = s.root().unwrap();
        // 8..12 and 10..40 merge to 8..len; 0..3 stands alone; the empty and
        // the inverted range are dropped by the `s < e` filter.
        assert_eq!(root.start, 0);
        assert_eq!(root.end, doc.len());
        assert_inside(&root, &[(0, 3), (8, doc.len())], "merged ranges");

        // An all-empty list is the whole document — tree-sitter's convention,
        // and the reason a consumer with no inline content should skip the
        // second pass instead of calling this with an empty list.
        let mut whole = Stream::new(Lang::MarkdownInline);
        whole.set_included_ranges(&[(5, 5)]);
        whole.push(doc);
        assert_eq!(whole.root().unwrap().end, doc.len());
    }

    // ---------- §3.3 error recovery ----------

    #[test]
    fn error_recovery_rust() {
        let mut s = Stream::new(Lang::Rust);
        s.push("fn main() { let x = ");
        let broken = s.root().expect("tree for the broken source");
        assert!(broken.has_error, "unfinished `let` should error");
        let err = find(&broken, "ERROR");
        assert!(!err.is_empty(), "an ERROR node is expected");

        // The header parsed before the error — the part incremental reuse is
        // supposed to keep.
        let header: Vec<(String, usize, usize)> = leaves_before(&broken, 11);

        s.push("1; }");
        let fixed = s.root().expect("tree for the fixed source");
        assert!(!fixed.has_error, "the finished function should be clean");
        assert!(find(&fixed, "ERROR").is_empty());
        assert_eq!(
            leaves_before(&fixed, 11),
            header,
            "the function header must be untouched by the append"
        );
        // And it really is one function now, not an error node holding the
        // pieces.
        let f = find(&fixed, "function_item");
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].start, f[0].end), (0, 24));

        fn leaves_before(n: &Node, byte: usize) -> Vec<(String, usize, usize)> {
            let mut out = Vec::new();
            fn walk(n: &Node, byte: usize, out: &mut Vec<(String, usize, usize)>) {
                if n.end > byte {
                    return;
                }
                if n.children.is_empty() {
                    out.push((n.kind.clone(), n.start, n.end));
                }
                for c in &n.children {
                    walk(c, byte, out);
                }
            }
            walk(n, byte, &mut out);
            out
        }
    }

    #[test]
    fn unterminated_markdown_fence_settles_then_completes() {
        // NOTE: the brief expected an error/missing node here. tree-sitter-md
        // does not produce one: CommonMark lets a fence be closed by the end
        // of the document, so the open fence is a *complete* `fenced_code_block`
        // with no trailing delimiter. What is worth pinning is exactly that —
        // no error, content running to EOF — and that the closing fence then
        // arrives as a second delimiter without disturbing the earlier blocks.
        let mut s = Stream::new(Lang::Markdown);
        s.push("text\n\n```rust\nfn f() {}\n");
        let open = s.root().expect("tree");
        assert!(!open.has_error);
        let block = find(&open, "fenced_code_block");
        assert_eq!(block.len(), 1);
        assert_eq!(
            find(block[0], "fenced_code_block_delimiter").len(),
            1,
            "only the opening delimiter exists yet"
        );
        let content = find(block[0], "code_fence_content");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].end, 24, "content runs to the end of the text");

        // The earlier paragraph is recorded before the append, so the append
        // can be shown not to have moved it.
        let para_before: Vec<(usize, usize)> = find(&open, "paragraph")
            .iter()
            .map(|n| (n.start, n.end))
            .collect();

        s.push("```\n");
        let closed = s.root().expect("tree after the closing fence");
        assert!(!closed.has_error);
        let block = find(&closed, "fenced_code_block");
        assert_eq!(block.len(), 1);
        assert_eq!(
            find(block[0], "fenced_code_block_delimiter").len(),
            2,
            "the closing delimiter arrived"
        );
        assert_eq!(
            find(&closed, "paragraph")
                .iter()
                .map(|n| (n.start, n.end))
                .collect::<Vec<_>>(),
            para_before,
            "the append must not disturb the blocks before it"
        );
    }

    // ---------- the rest of the API ----------

    #[test]
    fn root_is_none_before_the_first_push() {
        let mut s = Stream::new(Lang::Rust);
        assert!(s.root().is_none());
        assert!(s.captures("(identifier) @id").is_empty());
        assert_eq!(s.src(), "");
        assert_eq!(s.parse_calls(), 0);

        s.push("fn main() {}\n");
        assert!(s.root().is_some());
        assert_eq!(s.parse_calls(), 1);

        // An empty delta on a stream that already has a tree for this source
        // is a no-op: no parse, same tree.
        let before = s.root().unwrap();
        s.push("");
        assert_eq!(s.parse_calls(), 1);
        assert_eq!(s.root().unwrap(), before);
        assert_eq!(s.src(), "fn main() {}\n");
    }

    #[test]
    fn captures_come_back_as_plain_data_in_document_order() {
        let doc = "fn one() {}\nfn two() {}\n";
        let mut s = Stream::new(Lang::Rust);
        s.push(doc);
        let caps = s.captures("(identifier) @name");
        let got: Vec<(&str, &str)> = caps
            .iter()
            .map(|c| (c.name.as_str(), &doc[c.start..c.end]))
            .collect();
        assert_eq!(got, [("name", "one"), ("name", "two")]);

        // Cached: the same query twice is the same answer.
        assert_eq!(s.captures("(identifier) @name").len(), 2);
        // A query that does not compile is empty, not a panic.
        assert!(s.captures("(no_such_node) @x").is_empty());
        // And the stream still works afterwards (the cache was invalidated,
        // not poisoned).
        assert_eq!(s.captures("(identifier) @name").len(), 2);
    }

    #[test]
    fn captures_follow_the_growing_tree() {
        let mut s = Stream::new(Lang::Rust);
        s.push("fn one() {}\n");
        assert_eq!(s.captures("(identifier) @name").len(), 1);
        s.push("fn two() {}\n");
        let caps = s.captures("(identifier) @name");
        assert_eq!(caps.len(), 2);
        assert_eq!(&s.src()[caps[1].start..caps[1].end], "two");
    }

    // ---------- §3.5 the measurement ----------

    /// Median and p95 of per-push durations, in nanoseconds.
    fn median_p95(mut ns: Vec<u128>) -> (u128, u128) {
        ns.sort_unstable();
        let med = ns[ns.len() / 2];
        let p95 = ns[(ns.len() * 95 / 100).min(ns.len() - 1)];
        (med, p95)
    }

    /// Print one row of the measurement for the first and last 100 pushes and
    /// return those two `(median, p95)` pairs, in nanoseconds.
    fn report(label: &str, times: &[u128]) -> ((u128, u128), (u128, u128)) {
        let first = median_p95(times[..100].to_vec());
        let last = median_p95(times[times.len() - 100..].to_vec());
        println!(
            "{label:<22} pushes={:<5} | first100 med={:>8.1} µs p95={:>8.1} µs | last100 med={:>8.1} µs p95={:>8.1} µs | total={:>8.1} ms",
            times.len(),
            first.0 as f64 / 1e3,
            first.1 as f64 / 1e3,
            last.0 as f64 / 1e3,
            last.1 as f64 / 1e3,
            times.iter().sum::<u128>() as f64 / 1e6,
        );
        (first, last)
    }

    /// Push `doc` into `s` in `n` pieces, timing every push.
    fn timed_pushes(s: &mut Stream, doc: &str, n: usize, seed: u64) -> Vec<u128> {
        let cuts = cut_points(doc, n, &mut Rng(seed));
        let mut times = Vec::with_capacity(cuts.len());
        let mut at = 0;
        for &cut in &cuts {
            if cut <= at {
                continue;
            }
            let t = Instant::now();
            s.push(&doc[at..cut]);
            times.push(t.elapsed().as_nanos());
            at = cut;
        }
        assert_eq!(s.src(), doc);
        assert_eq!(s.parse_calls() as usize, times.len(), "one parse per push");
        times
    }

    /// The cost of parsing `doc` from scratch, in nanoseconds.
    fn one_shot(lang: Lang, doc: &str) -> u128 {
        let mut s = Stream::new(lang);
        let t = Instant::now();
        s.push(doc);
        t.elapsed().as_nanos()
    }

    /// The always-on half of the §3.5 measurement: cheap enough for every
    /// `cargo test`, and it asserts the property a consumer can rely on —
    /// an incremental push is never worse than parsing the document from
    /// scratch, and where tree-sitter *can* reuse nodes it is much better.
    #[test]
    fn an_incremental_push_beats_a_full_reparse() {
        // Rust: reuse works, so the last pushes stay far below a full parse.
        let doc = big_rust();
        let mut s = Stream::new(Lang::Rust);
        let times = timed_pushes(&mut s, &doc, 400, 0x1111_2222_3333_4444);
        let (_, last) = report("rust (block)", &times);
        let full = one_shot(Lang::Rust, &doc);
        println!(
            "rust: one-shot full parse of {} KB = {:.1} µs",
            doc.len() / 1000,
            full as f64 / 1e3
        );
        assert!(
            last.1 * 4 < full,
            "a late push should cost well under a full parse: p95={} ns full={} ns",
            last.1,
            full
        );

        // Markdown: reuse is refused by the platform (see the ignored
        // `per_push_cost_stays_flat`), so the honest assertion is the weaker
        // one — an append is not *worse* than re-parsing everything.
        let doc = md_doc();
        let mut s = Stream::new(Lang::Markdown);
        let times = timed_pushes(&mut s, &doc, 200, 0x5555_6666_7777_8888);
        let (_, last) = report("markdown (block)", &times);
        let full = one_shot(Lang::Markdown, &doc);
        println!(
            "markdown: one-shot full parse of {} KB = {:.1} µs",
            doc.len() / 1000,
            full as f64 / 1e3
        );
        assert!(
            last.1 * 2 < full * 3,
            "an append should not cost more than re-parsing the document: p95={} ns full={} ns",
            last.1,
            full
        );
    }

    /// Pins the platform behaviour this engine lives with today: the per-push
    /// cost grows with the document, so a stream of N pushes costs O(N²) in
    /// total. This is NOT what the brief asked for — the ignored
    /// `per_push_cost_stays_flat` states the brief's criterion and fails on
    /// it. The point of asserting it here is that a tree-sitter or grammar
    /// upgrade which *does* make appends cheap turns this test red, so the
    /// good news cannot go unnoticed.
    #[test]
    fn append_cost_is_currently_linear_in_the_document() {
        let doc = big_md();
        let mut per_push = Vec::new();
        for frac in [10usize, 40] {
            let n = snap(&doc, doc.len() * frac / 100);
            let mut s = Stream::new(Lang::Markdown);
            // Build to size `n` untimed, then time 64-byte appends.
            s.push(&doc[..n]);
            let mut times = Vec::new();
            let mut at = n;
            while at + 64 <= doc.len() && times.len() < 100 {
                let t = Instant::now();
                s.push(&doc[at..at + 64]);
                times.push(t.elapsed().as_nanos());
                at += 64;
            }
            let med = median_p95(times).0;
            println!(
                "markdown doc={n} B: 64 B append med={:.1} µs",
                med as f64 / 1e3
            );
            per_push.push(med);
        }
        let (small, big) = (per_push[0], per_push[1]);
        assert!(
            big > small + small / 2,
            "appends are expected to cost more as the document grows ({small} ns at 10%, {big} ns at 40%) — \
             if this fails, tree-sitter or the grammar got better and the note on `Stream` wants updating"
        );
    }

    /// The brief's §3.5 criterion, at the brief's size: a ~100 KB document
    /// pushed in 1,000 appends, asserting `p95(last 100) < 5 × median(first
    /// 100)`.
    ///
    /// **It fails today, and that is the finding.** Measured (release):
    /// markdown 829 µs → 9.5 ms per push (~11×, failing the 5× bound), the
    /// two-pass pipeline 5.4 ms → 88.8 ms (~16×). Per-push cost is linear in
    /// the document, so a stream's total cost is quadratic — see
    /// `Stream`'s note and TODO.md §9 for the diagnosis. It is `#[ignore]`d
    /// only because it takes ~40 s even in release (minutes in debug); run it
    /// with `cargo test --release --ignored --nocapture per_push_cost`.
    #[test]
    #[ignore = "slow (~40 s in release); documented failure, run explicitly"]
    fn per_push_cost_stays_flat() {
        for (label, lang, doc) in [
            ("markdown (block)", Lang::Markdown, big_md()),
            ("rust (block)", Lang::Rust, big_rust()),
        ] {
            assert!(doc.len() >= 90_000, "{label}: {} bytes", doc.len());
            let mut s = Stream::new(lang);
            let times = timed_pushes(&mut s, &doc, 1000, 0x9e37_79b9_7f4a_7c15);
            let (first, last) = report(label, &times);
            assert!(
                last.1 < 5 * first.0.max(1),
                "{label}: per-push cost is not flat — p95(last 100)={} ns >= 5 × median(first 100)={} ns",
                last.1,
                first.0
            );
        }
    }

    /// The harder half of §3.5: the secondary stream whose included ranges
    /// grow with every push — does the reuse stay honest when the range list
    /// itself keeps changing?
    #[test]
    #[ignore = "slow (~30 s in release); documented failure, run explicitly"]
    fn inline_pass_cost_stays_flat() {
        let doc = big_md();
        let mut block = Stream::new(Lang::Markdown);
        let mut inline = Stream::new(Lang::MarkdownInline);
        let cuts = cut_points(&doc, 1000, &mut Rng(0x2545_f491_4f6c_dd1d));
        let mut times = Vec::with_capacity(cuts.len());
        let mut at = 0;
        for &cut in &cuts {
            if cut <= at {
                continue;
            }
            let delta = &doc[at..cut];
            let t = Instant::now();
            block.push(delta);
            let ranges = inline_ranges(&block.root().expect("block tree"));
            inline.set_included_ranges(&ranges);
            inline.push(delta);
            times.push(t.elapsed().as_nanos());
            at = cut;
        }
        let (first, last) = report("two-pass pipeline", &times);
        assert!(
            last.1 < 5 * first.0.max(1),
            "two-pass pipeline: per-push cost is not flat — p95(last 100)={} ns >= 5 × median(first 100)={} ns",
            last.1,
            first.0
        );

        // And the result is still right, not just fast.
        let ranges = inline_ranges(&block.root().unwrap());
        let root = inline.root().expect("inline tree");
        assert_inside(&root, &ranges, "two-pass final tree");
        assert!(!find(&root, "strong_emphasis").is_empty());
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn a_token_names_a_language_and_a_miss_is_none() {
        assert_eq!(Lang::from_token("rust"), Some(Lang::Rust));
        assert_eq!(Lang::from_token("RUST"), Some(Lang::Rust));
        assert_eq!(Lang::from_token("tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_token("ts"), Some(Lang::TypeScript));
        assert_eq!(Lang::from_token("sh"), Some(Lang::Bash));
        assert_eq!(Lang::from_token("python3"), Some(Lang::Python));
        assert_eq!(Lang::from_token("diff"), Some(Lang::Diff));
        assert_eq!(Lang::from_token("elisp"), Some(Lang::Elisp));
        // An info string may carry a title or options after the language.
        assert_eq!(Lang::from_token("rust,ignore"), Some(Lang::Rust));
        assert_eq!(Lang::from_token("python title=\"x\""), Some(Lang::Python));
        assert_eq!(Lang::from_token("  yaml  "), Some(Lang::Yaml));
        // A language rano has no grammar for is not guessed at.
        for miss in ["text", "console", "output", "", "  ", "brainfuck", "nix"] {
            assert_eq!(Lang::from_token(miss), None, "{miss:?}");
        }
    }

    /// The token table and the extension table are **not** the same table, and this is
    /// the one entry where that is visible from an extension: `mk` is Make as a file
    /// extension and is not what anyone writes in a fence.
    #[test]
    fn the_token_table_is_not_the_extension_table() {
        assert_eq!(detect(Some(Path::new("x.mk")), None), Some(Lang::Make));
        assert_eq!(Lang::from_token("mk"), None);
        assert_eq!(Lang::from_token("make"), Some(Lang::Make));
        assert_eq!(Lang::from_token("makefile"), Some(Lang::Make));
        // And the other way: `console` is a fence people write and not an extension.
        assert_eq!(Lang::from_token("console"), None);
        assert_eq!(detect(Some(Path::new("x.console")), None), None);
    }
}

#[cfg(test)]
mod stream_spans_tests {
    use super::*;

    /// **The property an embedder depends on**: a text pushed a token at a time gives
    /// the same spans as the same text pushed whole.
    ///
    /// The two take different routes — the incremental parse reuses a prefix, the whole
    /// push does not — so this is the assertion that the reuse is not lossy for
    /// highlighting.
    #[test]
    fn a_growing_stream_ends_at_the_same_spans() {
        let src = "fn main() {\n    let xs: Vec<u32> = (0..5).collect();\n}\n";
        let mut grown = Stream::new(Lang::Rust);
        for chunk in src.as_bytes().chunks(7) {
            grown.push(std::str::from_utf8(chunk).unwrap());
        }
        let mut whole = Stream::new(Lang::Rust);
        whole.push(src);
        assert_eq!(grown.spans(), whole.spans());
        assert!(!whole.spans().is_empty(), "nothing was captured");
    }

    /// The spans say what the source says: a keyword is a keyword, on the right row,
    /// over the right characters.
    ///
    /// The names are the query's, so this asserts by *position* and by set — checking a
    /// literal capture name would be asserting the Rust query's spelling, which is
    /// upstream's to change.
    #[test]
    fn the_spans_cover_the_tokens_where_they_are() {
        let src = "fn main() {\n    let n = 1;\n}\n";
        let mut stream = Stream::new(Lang::Rust);
        stream.push(src);
        let spans = stream.spans();
        let on = |row: usize, text: &str| {
            let line = src.split('\n').nth(row).unwrap();
            line.find(text).unwrap()
        };
        // `fn` is on row 0 at char 0, and something captured it.
        assert!(
            spans
                .iter()
                .any(|s| s.row == 0 && s.start == 0 && s.end == 2),
            "`fn` is not covered: {spans:?}"
        );
        // `let` is on row 1, after the indent.
        let col = on(1, "let");
        assert!(
            spans
                .iter()
                .any(|s| s.row == 1 && s.start == col && s.end == col + 3),
            "`let` is not covered at {col}: {spans:?}"
        );
        // Nothing on an empty last row, and no row index out of range.
        assert!(spans.iter().all(|s| s.row < 4), "{spans:?}");
    }

    /// Multi-byte text is counted in **characters**, since that is what a terminal
    /// paints in, and a column that was a byte offset would land mid-glyph.
    ///
    /// The line is `    let s = "日本語";` — 18 chars, 25 bytes. The string's capture
    /// must end at char 17, not at byte 23, and nothing may report a column past the
    /// line's char count.
    #[test]
    fn columns_are_characters_not_bytes() {
        let src = "fn main() {\n    let s = \"日本語\";\n}\n";
        let mut stream = Stream::new(Lang::Rust);
        stream.push(src);
        let spans = stream.spans();
        let line = src.split('\n').nth(1).unwrap();
        let chars = line.chars().count();
        assert_eq!(chars, 18, "the fixture's own arithmetic");
        assert!(
            line.len() > chars,
            "the fixture must have a byte/char split"
        );
        let string = spans
            .iter()
            .find(|s| s.row == 1 && s.name == "string")
            .unwrap_or_else(|| panic!("no string span: {spans:?}"));
        assert_eq!((string.start, string.end), (12, 17), "{string:?}");
        // As byte offsets they would be 12 and 23 — the second is past the line, which
        // is the bug this pins.
        assert!(
            spans.iter().all(|s| s.row != 1 || s.end <= chars),
            "{spans:?}"
        );
    }

    /// Before a push, and for a language with no query, spans are empty rather than a
    /// panic.
    #[test]
    fn an_empty_stream_has_no_spans() {
        let mut stream = Stream::new(Lang::Rust);
        assert!(stream.spans().is_empty());
        stream.push("");
        assert!(stream.spans().is_empty());
    }

    /// Every language rano can name by token can also be highlighted from a stream.
    ///
    /// Swept rather than sampled because the failure this guards is a *missing query*,
    /// which only shows up for the languages nobody tested — and 28 of them is few
    /// enough to check all.
    #[test]
    fn every_language_highlighted_from_a_stream_colours_something() {
        // A snippet per grammar that the grammar is certain to capture something in.
        let sample = "let x = 1; // c\n";
        for (token, expect_in) in [
            ("rust", "let"),
            ("python", "def"),
            ("javascript", "const"),
            ("typescript", "const"),
            ("tsx", "const"),
            ("go", "func"),
            ("c", "int"),
            ("bash", "echo"),
            ("json", "a"),
            ("yaml", "a"),
            ("toml", "a"),
            ("html", "p"),
            ("css", "a"),
            ("lua", "local"),
            ("ruby", "def"),
            ("php", "echo"),
            ("java", "int"),
            ("sql", "select"),
            ("diff", "---"),
            ("markdown", "#"),
        ] {
            let lang = Lang::from_token(token).unwrap_or_else(|| panic!("{token}"));
            let src = match token {
                "python" => "def f():\n    pass\n",
                "javascript" | "typescript" | "tsx" => "const x = 1;\n",
                "go" => "func main() {}\n",
                "c" | "java" => "int main() {}\n",
                "bash" => "echo hi\n",
                "json" => "{\"a\": 1}\n",
                "yaml" => "a: 1\n",
                "toml" => "a = 1\n",
                "html" => "<p>a</p>\n",
                "css" => "a { color: red; }\n",
                "lua" => "local x = 1\n",
                "ruby" => "def f\nend\n",
                "php" => "<?php echo 1;\n",
                "sql" => "select 1;\n",
                "diff" => "--- a\n+++ b\n",
                "markdown" => "# a\n",
                _ => sample,
            };
            let mut stream = Stream::new(lang);
            stream.push(src);
            let spans = stream.spans();
            assert!(!spans.is_empty(), "{token} captured nothing from {src:?}");
            let covered = |probe: &str| {
                src.lines().enumerate().any(|(row, line)| {
                    line.find(probe).is_some_and(|col| {
                        let col = line[..col].chars().count();
                        spans.iter().any(|s| {
                            s.row == row && s.start <= col && s.end >= col + probe.chars().count()
                        })
                    })
                })
            };
            assert!(
                covered(expect_in),
                "{token}: `{expect_in}` is not covered: {spans:?}"
            );
        }
    }

    /// **The measurement R18.1's decision rests on.** A code fence grows a token at a
    /// time and the walk runs per push, so what decides whether an embedder can afford
    /// it is whether the per-push cost is a frame's budget at fence sizes.
    ///
    /// Ignored because it is slow; run with
    /// `cargo test -p rano --release --lib a_per_push -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_per_push_cost_is_a_frames_budget() {
        for lang in [Lang::Rust, Lang::Markdown] {
            let body = match lang {
                Lang::Markdown => "## heading\n\nsome prose with `code` in it\n\n",
                _ => "fn f() {\n    let n = 1;\n}\n\n",
            };
            let doc: String = body.repeat(200); // ~10 KB
            let tokens: Vec<&str> = doc.split_inclusive(' ').collect();
            let mut stream = Stream::new(lang);
            let mut first = std::time::Duration::ZERO;
            let mut last = std::time::Duration::ZERO;
            let per = tokens.len() / 10;
            for (i, t) in tokens.iter().enumerate() {
                let t0 = std::time::Instant::now();
                stream.push(t);
                let _ = stream.spans();
                let dt = t0.elapsed();
                if i < per {
                    first += dt;
                }
                if i >= tokens.len() - per {
                    last += dt;
                }
            }
            let early_us = first.as_secs_f64() * 1e6 / per as f64;
            let late_us = last.as_secs_f64() * 1e6 / per as f64;
            eprintln!(
                "{lang:?}: {} bytes, {} pushes; per-push early {early_us:.1} us, late {late_us:.1} us",
                doc.len(),
                tokens.len(),
            );
            assert!(
                last > first,
                "{lang:?}: a longer text must cost more per push"
            );
            assert!(
                late_us < 10_000.0,
                "{lang:?}: a push cost a millisecond or more"
            );
        }
    }

    /// The walk itself, without the push: what a renderer pays to repaint a settled
    /// fence. Ignored, `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_settled_walk_is_cheap_enough_to_repaint() {
        for (name, doc) in [
            ("700 lines", "let n = 1;\n".repeat(700)),
            ("1 line", format!("let n = 1;{}\n", " ".repeat(9_000))),
        ] {
            let mut stream = Stream::new(Lang::Rust);
            stream.push(&doc);
            let t = std::time::Instant::now();
            let rounds = 50;
            for _ in 0..rounds {
                let _ = stream.spans();
            }
            eprintln!(
                "{name}: {} bytes, {:.0} us per walk",
                doc.len(),
                t.elapsed().as_secs_f64() * 1e6 / rounds as f64
            );
        }
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    /// **Every name a language shows is a name it answers to.** The two tables would
    /// otherwise drift, and the symptom would be a status bar that says `typescript`
    /// beside a fence a reader cannot reproduce.
    #[test]
    fn every_name_is_a_token_the_table_knows() {
        for lang in [
            Lang::Rust,
            Lang::Go,
            Lang::Bash,
            Lang::Python,
            Lang::C,
            Lang::Json,
            Lang::CommonLisp,
            Lang::JavaScript,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::Toml,
            Lang::Yaml,
            Lang::Html,
            Lang::Css,
            Lang::Lua,
            Lang::Ruby,
            Lang::Php,
            Lang::Java,
            Lang::Make,
            Lang::Dockerfile,
            Lang::Ini,
            Lang::Diff,
            Lang::Elisp,
            Lang::Scheme,
            Lang::Sql,
            Lang::Clojure,
        ] {
            let name = lang.name();
            assert_eq!(
                Lang::from_token(name),
                Some(lang),
                "{name} does not name {lang:?}"
            );
        }
        // And the two grammar-only members are named too, even though a fence cannot ask
        // for them: `MarkdownInline` is driven by rano's own two-pass markdown consumer.
        assert_eq!(Lang::Markdown.name(), "markdown");
        assert_eq!(Lang::MarkdownInline.name(), "markdown-inline");
        assert_eq!(Lang::from_token("markdown-inline"), None);
    }
}

#[cfg(test)]
mod node_shape_tests {
    use super::*;

    /// **A node carries the field it was found under**, which is how a consumer asks a
    /// grammar-dependent question ("what is this declaration's name") without
    /// re-deriving it from child order.
    #[test]
    fn a_child_knows_its_field() {
        let mut stream = Stream::new(Lang::Rust);
        stream.push("fn main() { let n = 1; }\n");
        let root = stream.root().unwrap();
        // `function_item` has a `name` field, and the name is an identifier.
        fn find<'a>(n: &'a Node, kind: &str) -> Option<&'a Node> {
            if n.kind == kind {
                return Some(n);
            }
            n.children.iter().find_map(|c| find(c, kind))
        }
        let f = find(&root, "function_item").expect("a function_item");
        let name = f
            .children
            .iter()
            .find(|c| c.field.as_deref() == Some("name"))
            .expect("a child in the `name` field");
        assert_eq!(name.kind, "identifier");
        assert_eq!(name.start, 3, "the name is `main` at byte 3");
        // And the field is a name, not a kind: the parentheses are children too.
        assert!(
            f.children.iter().any(|c| c.field.is_none()),
            "{:#?}",
            f.children
        );

        // The root is under no field.
        assert_eq!(root.field, None);
    }

    /// A node's points are its byte offsets as row/column, in bytes, zero-based — and
    /// rano's own type, so a consumer reads a coordinate without depending on a parser.
    #[test]
    fn a_node_carries_its_position() {
        let mut stream = Stream::new(Lang::Rust);
        stream.push("fn main() {\n    let n = 1;\n}\n");
        let root = stream.root().unwrap();
        fn find<'a>(n: &'a Node, kind: &str) -> Option<&'a Node> {
            if n.kind == kind {
                return Some(n);
            }
            n.children.iter().find_map(|c| find(c, kind))
        }
        let n = find(&root, "integer_literal").expect("the 1");
        assert_eq!(n.start_point.row, 1, "second line");
        assert_eq!(
            n.start_point.column, 12,
            "byte column, after four spaces of indent"
        );
        assert_eq!(n.end_point.column, 13);
        // And rano's Point round-trips into tree-sitter's, which is what a parser needs.
        let ts: tree_sitter::Point = n.start_point.into();
        assert_eq!(ts.row, 1);
    }

    /// The multi-byte case: the column is **bytes**, so it matches the byte offsets
    /// beside it rather than disagreeing with them.
    #[test]
    fn a_column_is_bytes_not_characters() {
        let mut stream = Stream::new(Lang::Rust);
        stream.push("// 日本語\nlet n = 1;\n");
        let root = stream.root().unwrap();
        fn find<'a>(n: &'a Node, kind: &str) -> Option<&'a Node> {
            if n.kind == kind {
                return Some(n);
            }
            n.children.iter().find_map(|c| find(c, kind))
        }
        let n = find(&root, "integer_literal").expect("the 1");
        let line_start = "// 日本語\n".len();
        assert_eq!(
            n.start,
            line_start + 8,
            "the byte offset is the source of truth"
        );
        assert_eq!(n.start_point.row, 1);
        assert_eq!(n.start_point.column, 8, "bytes into the second line");
    }
}

#[cfg(test)]
mod node_equality_tests {
    use super::*;

    /// **Two parses of one text are the same tree**, which is what the streaming
    /// property rests on. [`Node::id`] differs between them — it is tree-sitter's
    /// per-tree pointer — so equality deliberately leaves it out, and this is the test
    /// that says so. It was caught by `streaming_equals_one_push_for_*` the day `id` was
    /// added with a derived `PartialEq`.
    #[test]
    fn equality_is_the_value_and_not_the_identity() {
        let src = "fn main() {\n    let n = 1;\n}\n";
        let mut a = Stream::new(Lang::Rust);
        a.push(src);
        let mut b = Stream::new(Lang::Rust);
        b.push(src);
        let (ta, tb) = (a.root().unwrap(), b.root().unwrap());
        assert_eq!(ta, tb, "the same text is the same tree");
        // The identity is still available, and it is *not* the value.
        assert_ne!(ta.id, tb.id, "two parses have different node ids");
        assert_ne!(ta, {
            let mut c = Stream::new(Lang::Rust);
            c.push("fn other() {}\n");
            c.root().unwrap()
        });
    }
}

#[cfg(test)]
mod query_cache_tests {
    use super::*;

    /// **A query is compiled once per process per language.**
    ///
    /// This is the regression for the startup cliff: `Query::new` is 8.7 ms for Rust and
    /// 10.2 ms for TSX (measured 2026-09-20), and a renderer that compiles one per code
    /// block pays that per block. `Arc::ptr_eq` is the assertion that says *shared*
    /// rather than *fast* — a timing test would pass on a fast machine with the bug
    /// still in it.
    #[test]
    fn a_query_is_compiled_once_and_shared() {
        let a = highlight_query(Lang::Rust).expect("rust has a query");
        let b = highlight_query(Lang::Rust).expect("still");
        assert!(Arc::ptr_eq(&a, &b), "the second call recompiled");

        // A different language is a different query.
        let other = highlight_query(Lang::Python).expect("python has a query");
        assert!(!Arc::ptr_eq(&a, &other));

        // And `None` is cached too, so a language whose query does not compile does not
        // re-parse its `.scm` on every call. There is no such language today — every
        // grammar's query compiles — so this asserts the shape of the code path rather
        // than a case: the cache is keyed by language and stores the answer either way.
        assert!(highlight_query(Lang::Rust).is_some());
    }

    /// The two consumers reach that one cache: a `Highlighter` and a `Stream` of the
    /// same language share the query rather than each holding its own.
    #[test]
    fn both_consumers_share_the_one_query() {
        // A `Stream`'s spans and a `Highlighter`'s classes are the same walk over the
        // same query; the only way to see the sharing from outside is that neither
        // compiles, which the pointer test above covers. What this asserts is the
        // *observable* agreement, which would break if either had its own copy.
        let src = "fn main() {\n    let n = 1;\n}\n";
        let mut stream = Stream::new(Lang::Rust);
        stream.push(src);
        let spans = stream.spans();
        let mut hl = Highlighter::new();
        let classes = hl.classes(src, Lang::Rust);
        // Every span's characters are covered in the grid by the same capture name.
        for s in &spans {
            let row = &classes[s.row];
            for col in s.start..s.end {
                assert_eq!(
                    row.get(col).and_then(|c| c.as_deref()),
                    Some(s.name.as_str()),
                    "row {} col {col} disagrees between spans and classes",
                    s.row
                );
            }
        }
    }

    /// The measurement the fix was made against. Ignored because timing is not an
    /// assertion; run with `--ignored --nocapture` to see it.
    #[test]
    #[ignore]
    fn the_first_use_of_a_language_pays_and_the_rest_do_not() {
        for (name, lang, body) in [
            ("rust", Lang::Rust, "fn main() {\n    let n = 1;\n}\n"),
            ("tsx", Lang::Tsx, "const A = () => <div>hi</div>;\n"),
            ("diff", Lang::Diff, "--- a\n+++ b\n"),
        ] {
            let mut first = std::time::Duration::ZERO;
            let mut rest = std::time::Duration::ZERO;
            let rounds = 40;
            for i in 0..rounds {
                let t = std::time::Instant::now();
                let mut s = Stream::new(lang);
                s.push(body);
                let _ = s.spans();
                if i == 0 {
                    first = t.elapsed();
                } else {
                    rest += t.elapsed();
                }
            }
            eprintln!(
                "{name:<5} first {:>8.1} us, each after {:>6.1} us",
                first.as_secs_f64() * 1e6,
                rest.as_secs_f64() * 1e6 / (rounds - 1) as f64
            );
        }
    }
}

/// Markdown's two-grammar highlighting: the block pass does structure, the
/// inline pass does emphasis/code/links, and they overlay one grid.
#[cfg(test)]
mod markdown_tests {
    use super::*;
    use std::time::Instant;

    /// The two-pass markdown highlight must be LINEAR in the document, which
    /// is the property that matters and the one an absolute stopwatch cannot
    /// state: a wall-clock bound here flaked under the suite's own parallelism.
    /// Doubling the document must roughly double the work, not square it.
    #[test]
    fn the_markdown_two_pass_is_linear_in_the_document() {
        let build = |paras: usize| {
            let mut src = String::new();
            for i in 0..paras {
                src.push_str(&format!(
                    "Paragraph {i} with **bold** and `code` and a [link](http://x/{i}).\n\n"
                ));
            }
            let mut buf = Buffer::new();
            buf.lines = src.lines().map(|l| l.chars().collect()).collect();
            buf.name = Some(std::path::PathBuf::from("x.md"));
            buf
        };
        let time = |buf: &Buffer| {
            let mut hl = Highlighter::new();
            hl.refresh(buf); // warm the query and the tree
            let t = Instant::now();
            for _ in 0..3 {
                hl.refresh(buf);
            }
            t.elapsed().as_secs_f64() / 3.0
        };
        let small = build(300);
        let big = build(1_200);
        assert!(big.lines.len() == small.lines.len() * 4);
        let (a, b) = (time(&small), time(&big));
        println!(
            "markdown refresh: {:.1} ms at {} lines, {:.1} ms at {} lines ({:.1}× for 4×)",
            a * 1e3,
            small.lines.len(),
            b * 1e3,
            big.lines.len(),
            b / a
        );
        // 4× the document: quadratic would be 16×, so a bound of 8× separates
        // the two by a wide margin while tolerating a loaded machine.
        assert!(
            b < a * 8.0,
            "the two-pass highlight looks quadratic: {a:.4} s → {b:.4} s for 4× the text"
        );
    }

    /// Every parse the inline pass hands out covers exactly ONE node's ranges.
    ///
    /// This is the invariant that keeps a delimiter from pairing across
    /// paragraphs. It watches the trees the pass actually produces, so it
    /// fails if the pass is ever changed to hand the grammar several nodes'
    /// ranges at once — which is the shape of the bug: on it, one parse
    /// covered 12 KB of a 33 KB document and painted everything after it.
    fn assert_pass_is_per_node(src: &str) {
        let mut parser = Parser::new();
        parser.set_language(&Lang::Markdown.language()).unwrap();
        let block = parser.parse(src.as_bytes(), None).unwrap();
        let groups = markdown_inline_node_ranges(&block);
        assert!(!groups.is_empty(), "the fixture has no inline content");
        // Within a group: sorted and disjoint (tree-sitter's own rule).
        for (i, g) in groups.iter().enumerate() {
            assert!(g.windows(2).all(|w| w[0].1 <= w[1].0), "group {i}: {g:?}");
        }
        let mut hl = Highlighter::new();
        // What each parse was handed: its own node's bytes, and the row base
        // the grid will offset by.
        let mut parsed: Vec<(usize, usize)> = Vec::new();
        let mut bases: Vec<usize> = Vec::new();
        hl.markdown_inline_pass(src, &block, |slice, base_row, _tree, _q| {
            parsed.push((slice.len(), 0));
            bases.push(base_row);
        });
        assert_eq!(
            parsed.len(),
            groups.len(),
            "one parse per node, no more and no fewer"
        );
        // THE invariant that keeps this linear: no parse is handed more than
        // its own node. Handing it the whole source with the node's ranges is
        // what made a 2,400-paragraph document cost 9.8× for 4× the text —
        // tree-sitter walks the excluded input to reach each range.
        let line_starts = {
            let mut v = vec![0usize];
            for (i, b) in src.bytes().enumerate() {
                if b == b'\n' {
                    v.push(i + 1);
                }
            }
            v
        };
        // The slice is grown to whole rows, so allow the slack to the end of
        // the node's last row; anything near the document's size is the bug.
        for (i, g) in groups.iter().enumerate() {
            let span = g.last().unwrap().1 - g.first().unwrap().0;
            // The same upper bound the pass uses: the start of the row after
            // the node's last row, so the slice can end on a whole line.
            let row_of = |b: usize| line_starts.partition_point(|&o| o <= b).saturating_sub(1);
            let row_end = line_starts
                .get(row_of(g.last().unwrap().1.saturating_sub(1)) + 1)
                .copied()
                .unwrap_or(src.len())
                .max(g.last().unwrap().1);
            // The slice starts at the beginning of the node's FIRST row, not
            // at the node, so the allowance is measured from there.
            let allowed = row_end - line_starts[row_of(g.first().unwrap().0)];
            assert!(
                parsed[i].0 <= allowed,
                "parse {i} was handed {} bytes for a node of {span} (allowed {allowed})",
                parsed[i].0
            );
        }
        // Rows ascend, and the first parse starts at the document's first row
        // when the first node does.
        assert!(
            bases.windows(2).all(|w| w[0] <= w[1]),
            "row bases: {bases:?}"
        );
        // And the flattened helper agrees with the groups.
        let flat = markdown_inline_ranges(&block);
        assert_eq!(flat.len(), groups.iter().map(|g| g.len()).sum::<usize>());
        assert!(
            flat.windows(2).all(|w| w[0].1 <= w[1].0),
            "flattened: {flat:?}"
        );
    }

    #[test]
    fn the_inline_pass_parses_one_node_at_a_time() {
        assert_pass_is_per_node(
            "# H\n\nfirst `code` here\n\nsecond **bold** here\n\n| a | b |\n| - | - |\n| 1 | 2 |\n",
        );
        assert_pass_is_per_node(
            "Para one ends with a lone backtick `\n\nanother `code` span here.\n\n` ```console ` and done.\n",
        );
    }

    /// The same invariant on the real document that exposed the bug — the one
    /// the operator had open. Skipped when it is not on this machine, so the
    /// suite stays self-contained.
    #[test]
    fn the_pass_is_per_node_on_the_parity_document() {
        let path = "/home/dead/Projects/head-parity-2026-09-21.md";
        let Ok(src) = std::fs::read_to_string(path) else {
            eprintln!("{path} is not here; skipping");
            return;
        };
        assert_pass_is_per_node(&src);
        // And no row is painted as one long code span. The symptom was 300-odd
        // cyan rows: everything from the stray backtick to the end of the file.
        let mut hl = Highlighter::new();
        let grid = hl.classes(&src, Lang::Markdown);
        for (r, line) in src.split('\n').enumerate() {
            let Some(row) = grid.get(r) else { continue };
            let len = line.chars().count();
            if len < 4 {
                continue;
            }
            let literal = row
                .iter()
                .filter(|c| c.as_deref() == Some("text.literal"))
                .count();
            // Indented code blocks are legitimately all-literal, and the
            // document has two (the A=/B= legend).
            let indented = line.starts_with("    ");
            assert!(
                literal < len || indented,
                "row {r} is entirely a code span: {:?}",
                &line[..line.len().min(50)]
            );
        }
    }

    #[test]
    fn classes_sees_the_inline_constructs_too() {
        // The embedder's view (capture names, no palette) must agree with the
        // editor's: both run the two passes.
        let mut hl = Highlighter::new();
        let grid = hl.classes(DOC, Lang::Markdown);
        let name_at = |row: usize, text: &str, off: usize| {
            let col = DOC.split('\n').nth(row).unwrap().find(text).unwrap() + off;
            grid.get(row).and_then(|r| r.get(col)).cloned().flatten()
        };
        assert_eq!(name_at(2, "**bold**", 2).as_deref(), Some("text.strong"));
        assert_eq!(name_at(2, "`code`", 1).as_deref(), Some("text.literal"));
        assert_eq!(name_at(2, "*it*", 1).as_deref(), Some("text.emphasis"));
        assert_eq!(name_at(2, "[link]", 1).as_deref(), Some("text.reference"));
        assert_eq!(name_at(2, "~~strike~~", 2).as_deref(), Some("text.strike"));
        // The block half is still there.
        assert_eq!(name_at(0, "#", 0).as_deref(), Some("keyword"));
    }

    /// A document with one of each construct, at known rows.
    const DOC: &str = "\
# Head `x`

A para with **bold** and `code` and *it* and [link](http://x) and ~~strike~~.

> quote

- item

```rust
fn main() {}
```

<div>html</div>

---
";

    /// The style at the character `off` chars past `text` on `row`.
    fn style_in(src: &str, row: usize, text: &str, off: usize) -> Option<Style> {
        let mut buf = Buffer::new();
        buf.lines = src.lines().map(|l| l.chars().collect()).collect();
        buf.name = Some(std::path::PathBuf::from("x.md"));
        let mut hl = Highlighter::new();
        hl.refresh(&buf);
        let line = src.split('\n').nth(row).expect("row in range");
        let col = line.find(text).expect("probe text on the row") + off;
        hl.style_at(Pos { row, col })
    }

    #[test]
    fn inline_constructs_are_coloured_and_told_apart() {
        // The finding: the block grammar cannot see emphasis, code spans or
        // links, so before the inline pass every one of these was plain.
        let strong = style_in(DOC, 2, "**bold**", 2).expect("strong text is coloured");
        let code = style_in(DOC, 2, "`code`", 1).expect("a code span is coloured");
        let emphasis = style_in(DOC, 2, "*it*", 1).expect("emphasis is coloured");
        let link = style_in(DOC, 2, "[link]", 1).expect("link text is coloured");
        let url = style_in(DOC, 2, "http://x", 0).expect("a URL is coloured");
        let strike = style_in(DOC, 2, "~~strike~~", 2).expect("struck text is coloured");

        // Each is its own thing on screen. The four *kinds* are told apart,
        // as nano's markdown mode tells them apart; a link's text and its
        // destination deliberately share the link colour, because they are
        // one construct (nano colours an inline link all one colour too).
        let kinds = [
            ("strong", strong),
            ("code", code),
            ("emphasis", emphasis),
            ("strike", strike),
        ];
        for (i, (an, a)) in kinds.iter().enumerate() {
            for (bn, b) in &kinds[i + 1..] {
                assert_ne!(a, b, "{an} and {bn} must look different");
            }
        }
        assert_eq!(link, url, "a link and its destination are one construct");
        for (name, style) in [("link", link), ("url", url)] {
            assert_ne!(style, strong, "{name} must not read as emphasis");
            assert_ne!(style, code, "{name} must not read as code");
        }
        // And no construct wears the delimiter's grey, or prose's default.
        let delimiter = style_in(DOC, 2, "**bold**", 0).expect("delimiters are coloured");
        for (name, style) in [
            ("strong", strong),
            ("code", code),
            ("emphasis", emphasis),
            ("link", link),
            ("strike", strike),
        ] {
            assert_ne!(style, delimiter, "{name} must not wear the delimiter grey");
            assert_ne!(
                style,
                Style::default(),
                "{name} must not read as plain prose"
            );
        }
        // And each carries the weight nano's markdown mode gives it.
        assert!(strong.add_modifier.contains(Modifier::BOLD), "strong");
        assert!(emphasis.add_modifier.contains(Modifier::ITALIC), "emphasis");
        assert!(
            strike.add_modifier.contains(Modifier::CROSSED_OUT),
            "strike"
        );
        assert_eq!(code.fg, theme("text.literal").fg, "code spans read as code");
    }

    #[test]
    fn the_two_passes_overlay_instead_of_clobbering() {
        // A heading's text is `type` yellow, but a code span inside it must
        // still be a code span: the inline pass runs second and wins on the
        // cells it covers, while the rest of the heading keeps its colour.
        let heading = style_in(DOC, 0, "Head", 0).expect("heading text is coloured");
        let inner_code = style_in(DOC, 0, "`x`", 1).expect("code inside a heading");
        assert_ne!(heading, inner_code, "the code span must not be swallowed");
        assert_eq!(inner_code.fg, theme("text.literal").fg);
        // The marker keeps the block pass's colour.
        assert_eq!(
            style_in(DOC, 0, "#", 0).map(|s| s.fg),
            Some(theme("keyword").fg),
        );
    }

    #[test]
    fn block_structure_still_works() {
        // The inline pass must not undo what the block pass established.
        assert_eq!(
            style_in(DOC, 4, "> quote", 0).map(|s| s.fg),
            Some(theme("punctuation.bracket").fg),
            "block quote marker",
        );
        assert_eq!(
            style_in(DOC, 6, "- item", 0).map(|s| s.fg),
            Some(theme("punctuation.bracket").fg),
            "list marker",
        );
        assert_eq!(
            style_in(DOC, 14, "---", 0).map(|s| s.fg),
            Some(theme("comment").fg),
            "thematic break",
        );
        // A fenced body reads as code (nano colours the whole fence).
        assert!(style_in(DOC, 9, "fn main", 0).is_some(), "fence body");
        // Raw HTML is a tag.
        assert_eq!(
            style_in(DOC, 12, "<div>html</div>", 0).map(|s| s.fg),
            Some(theme("tag").fg),
        );
    }

    #[test]
    fn block_continuation_indentation_is_not_painted() {
        // From the head-parity doc: `(block_continuation) @punctuation.bracket`
        // was in the query, and a block_continuation node spans the leading
        // whitespace of a continuation line — so the indent of one bullet's
        // second line painted while its neighbour's did not, and two adjacent
        // identical lines looked different. The rule is gone.
        let src = "- **A:** first line\n  continued here\n- **B:** another\n  also continued\n";
        let mut buf = Buffer::new();
        buf.lines = src.lines().map(|l| l.chars().collect()).collect();
        buf.name = Some(std::path::PathBuf::from("x.md"));
        let mut hl = Highlighter::new();
        hl.refresh(&buf);
        for row in [1usize, 3] {
            for col in 0..2 {
                assert_eq!(
                    hl.style_at(Pos { row, col }),
                    None,
                    "row {row} col {col}: continuation indentation is not content"
                );
            }
        }
        // …while the bullet markers themselves are still painted.
        assert!(hl.style_at(Pos { row: 0, col: 0 }).is_some());
        assert!(hl.style_at(Pos { row: 2, col: 0 }).is_some());
    }

    #[test]
    fn inline_ranges_cover_the_inline_text_and_skip_parsed_children() {
        let mut parser = Parser::new();
        parser.set_language(&Lang::Markdown.language()).unwrap();
        let tree = parser.parse(DOC.as_bytes(), None).unwrap();
        let ranges = markdown_inline_ranges(&tree);
        assert!(!ranges.is_empty());
        // Sorted and disjoint: tree-sitter's own requirement, enforced here so
        // a caller never has to.
        for w in ranges.windows(2) {
            assert!(w[0].1 <= w[1].0, "unsorted or overlapping: {ranges:?}");
        }
        // The `**bold**` text is inline content, so some range covers it.
        let at = DOC.find("bold").unwrap();
        assert!(
            ranges.iter().any(|(s, e)| *s <= at && at < *e),
            "no inline range covers the emphasis text"
        );
        // A heading's marker is block structure, not inline content.
        let hash = DOC.find('#').unwrap();
        assert!(
            !ranges.iter().any(|(s, e)| *s <= hash && hash < *e),
            "the heading marker must not be handed to the inline grammar"
        );
    }

    #[test]
    fn one_nodes_backtick_never_pairs_with_another_nodes() {
        // The bug this pins, found by running rano on a real 33 KB document:
        // the inline grammar treats the ranges it is handed as ONE
        // concatenated stream, so a backtick that cannot close inside its own
        // node pairs with one in a later node. Measured on that document, a
        // single stray `` ` ```console ` `` grew into a 12 KB code span and
        // painted every paragraph after it as code — the screen went cyan
        // from that line to the bottom.
        //
        // Two nodes, the first ending on a lone backtick. Combined, the only
        // closer available is the next node's, so the parse crosses the
        // boundary (that span is `` `\n\nanother ` ``). Per node it cannot,
        // which is why the pass parses per node.
        let src = "Para one ends with a lone backtick `\n\nanother `code` span here.\n";
        let mut hl = Highlighter::new();
        let grid = hl.classes(src, Lang::Markdown);
        let cap = |off: usize| -> Option<String> {
            grid.iter()
                .flat_map(|r| r.iter())
                .nth(off)
                .cloned()
                .flatten()
        };
        // Offsets of the two inline nodes, from the block tree.
        let mut parser = Parser::new();
        parser.set_language(&Lang::Markdown.language()).unwrap();
        let tree = parser.parse(src.as_bytes(), None).unwrap();
        let groups = markdown_inline_node_ranges(&tree);
        assert_eq!(groups.len(), 2, "two paragraphs, two inline nodes");
        let (second_start, _) = groups[1][0];
        assert_eq!(&src[second_start..second_start + 7], "another");
        // The prose of the second paragraph is NOT a code span.
        for off in second_start..second_start + 7 {
            assert_ne!(
                cap(off).as_deref(),
                Some("text.literal"),
                "offset {off} was swallowed by the previous node's backtick"
            );
        }
        // …and the second paragraph's own code span still works.
        let code = src.find("`code`").unwrap() + 1;
        assert_eq!(cap(code).as_deref(), Some("text.literal"));
    }
}

/// Windowed highlighting: for buffers big enough that a whole-document pass
/// would be felt per keystroke, only the viewport's rows are coloured. These
/// two tests are what makes the window safe to rely on: it agrees with a full
/// pass inside it, and its cost does not follow the document.
#[cfg(test)]
mod window_tests {
    use super::*;
    use std::time::Instant;

    /// A windowed highlight must agree with a whole-buffer one, row for row,
    /// for every row inside the window. The window exists to make large files
    /// affordable; it must not make them wrong.
    #[test]
    fn a_window_matches_a_full_highlight_inside_it() {
        let mut src = String::new();
        for i in 0..400 {
            src.push_str(&format!(
                "/// doc {i}\npub fn f{i}(x: usize) -> usize {{ let y = x * {i}; y + 1 }}\n\n"
            ));
        }
        let mut buf = Buffer::new();
        buf.lines = src.lines().map(|l| l.chars().collect()).collect();
        buf.name = Some(std::path::PathBuf::from("big.rs"));

        let mut full = Highlighter::new();
        full.refresh(&buf);
        assert_eq!(full.styled_window(), None, "a full refresh styles row 0 up");

        for (first, last) in [(0usize, 20usize), (100, 140), (700, 799)] {
            let mut win = Highlighter::new();
            win.refresh_window(&buf, Window::rows(first, last));
            assert_eq!(
                win.styled_window(),
                Some(Window::rows(first, last)),
                "the window is recorded as asked for"
            );
            let mut styled = 0usize;
            for row in first..=last {
                let line = &buf.lines[row];
                for col in 0..line.len() {
                    let p = Pos { row, col };
                    assert_eq!(
                        win.style_at(p),
                        full.style_at(p),
                        "row {row} col {col} differs between the window and the full pass"
                    );
                    styled += 1;
                }
            }
            assert!(styled > 100, "the window actually styled something");
            // Outside the window: no answer, which the renderer reads as
            // uncoloured. Row `first - 1` is the interesting one.
            if first > 0 {
                assert_eq!(
                    win.style_at(Pos {
                        row: first - 1,
                        col: 0
                    }),
                    None
                );
            }
            assert_eq!(
                win.style_at(Pos {
                    row: last + 1,
                    col: 0
                }),
                None
            );
        }
    }

    /// A window can start and end MID-ROW, which is what makes a one-line
    /// document editable: minified JavaScript, one-line JSON and a minified
    /// CSS bundle are a single row megabytes long, and a row-shaped window
    /// over one of those is the whole file.
    #[test]
    fn a_window_can_be_part_of_a_single_enormous_row() {
        /// Columns at each end of the window where a crossing construct may
        /// differ from a full pass.
        const EDGE: usize = 32;
        // One row, long enough that a whole-row window would be the document.
        let row: String = (0..40_000)
            .map(|i| format!("function f{i}(a,b){{return a+b}}"))
            .collect();
        let mut buf = Buffer::new();
        buf.lines = vec![row.chars().collect()];
        buf.name = Some(std::path::PathBuf::from("bundle.js"));
        assert_eq!(buf.lines.len(), 1, "the document is one row");
        assert!(buf.lines[0].len() > 500_000, "and a big one");

        let mut full = Highlighter::new();
        full.refresh(&buf);

        // A window over 2,000 characters of it, 100,000 in.
        let win = Window::cols(0, 100_000, 102_000);
        assert!(win.char_count(buf.lines.len(), |r| buf.lines[r].len()) < 2_100);
        let mut hl = Highlighter::new();
        hl.refresh_window(&buf, win);
        assert_eq!(hl.styled_window(), Some(win));
        // Inside the window, away from its edges, the colours are exactly a
        // full pass's. That is the promise; the margin is what makes it true,
        // and it is asserted separately below rather than assumed.
        let interior = 100_000 + EDGE..102_000 - EDGE;
        for col in interior.clone() {
            let p = Pos { row: 0, col };
            assert_eq!(
                hl.style_at(p),
                full.style_at(p),
                "col {col} differs between the window and the full pass"
            );
        }
        assert!(interior.count() > 1_900, "most of the window is interior");
        // At the very edges a construct that opened outside the window is
        // coloured as though it opened here: that is the trade, and this is
        // the assertion that keeps it from being a surprise. The editor passes
        // a margin so these characters are not on screen.
        let edge_differs = (100_000..100_000 + EDGE)
            .chain(102_000 - EDGE..102_000)
            .filter(|c| {
                hl.style_at(Pos { row: 0, col: *c }) != full.style_at(Pos { row: 0, col: *c })
            })
            .count();
        println!(
            "{edge_differs} of {} edge columns differ (the documented trade)",
            EDGE * 2
        );
        // And outside the window there is no answer, so the renderer leaves
        // the rest of the row uncoloured rather than colouring it wrongly.
        assert_eq!(hl.style_at(Pos { row: 0, col: 0 }), None);
        assert_eq!(
            hl.style_at(Pos {
                row: 0,
                col: 102_001
            }),
            None
        );
    }

    /// The window parses only its own rows, so its cost must not track the
    /// document's size. Stated as a comparison rather than a stopwatch: the
    /// same 60 rows of a document four times longer must cost about the same.
    #[test]
    fn a_window_costs_the_window_not_the_document() {
        let build = |lines: usize| {
            let mut src = String::new();
            for i in 0..lines {
                src.push_str(&format!("pub fn f{i}(x: usize) -> usize {{ x + {i} }}\n"));
            }
            let mut buf = Buffer::new();
            buf.lines = src.lines().map(|l| l.chars().collect()).collect();
            buf.name = Some(std::path::PathBuf::from("big.rs"));
            buf
        };
        let small = build(2_000);
        let large = build(40_000);
        let time_window = |buf: &Buffer| {
            let mut hl = Highlighter::new();
            // Warm, then measure.
            hl.refresh_window(buf, Window::rows(1_000, 1_060));
            let t = Instant::now();
            for _ in 0..5 {
                hl.refresh_window(buf, Window::rows(1_000, 1_060));
            }
            t.elapsed().div_f64(5.0).as_secs_f64() / buf.lines.len() as f64
        };
        let a = time_window(&small);
        let b = time_window(&large);
        println!(
            "window cost per buffer row: {:.3} µs at 2k lines, {:.3} µs at 40k",
            a * 1e6,
            b * 1e6
        );
        // Per row, the 20× larger buffer must not be meaningfully more
        // expensive. A whole-document highlight would be ~20× here.
        assert!(
            b < a * 4.0 + 1e-9,
            "per-row window cost grew with the document: {a:.3e} → {b:.3e} s/row"
        );
    }
}
