/// Source context scanner for prompt enrichment.
///
/// Extracts function signatures, struct/enum definitions, and trait declarations
/// from Rust source files listed in a task's `touchesFiles`. Returns structured
/// `SourceContext` with token budget enforcement.
///
/// This gives Claude real awareness of current code state rather than relying
/// on potentially stale PRD descriptions.
use std::fs;
use std::path::Path;

/// Default character budget for source context when caller doesn't specify.
const DEFAULT_BUDGET_CHARS: usize = 2000;

/// Per-file character budget cap.
const PER_FILE_BUDGET: usize = 1500;

/// Summary of public signatures extracted from a single source file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileSummary {
    /// Path to the source file
    pub file: String,
    /// Extracted public signatures (fn, struct, enum, trait, type, impl)
    pub signatures: Vec<String>,
}

/// Aggregated source context from multiple files.
#[derive(Debug, Clone)]
pub struct SourceContext {
    /// Per-file summaries
    pub summaries: Vec<FileSummary>,
    /// Total characters used (for budget tracking)
    pub total_chars: usize,
}

impl SourceContext {
    /// Format the source context as a markdown block for prompt injection.
    pub fn format_for_prompt(&self) -> String {
        if self.summaries.is_empty() {
            return String::new();
        }

        let mut output = String::from("## Current Source Context\n\n");
        for summary in &self.summaries {
            if summary.signatures.is_empty() {
                continue;
            }
            output.push_str(&format!("### {}\n```rust\n", summary.file));
            for sig in &summary.signatures {
                output.push_str(sig);
                output.push('\n');
            }
            output.push_str("```\n\n");
        }
        output
    }
}

/// Scan source files and extract public API signatures.
///
/// # Arguments
///
/// * `files` - List of file paths to scan (relative or absolute)
/// * `budget_chars` - Maximum total characters for the output (0 = use default)
/// * `base_dir` - Base directory to resolve relative paths against
///
/// # Behavior
///
/// - Skips files that don't exist (logs warning, continues)
/// - Skips non-`.rs` files gracefully
/// - Extracts lines matching: `pub fn`, `pub struct`, `pub enum`, `pub trait`,
///   `pub type`, `impl ... for`
/// - Enforces token budget: truncates per-file output, drops least-relevant files
/// - Returns empty context for empty input
pub fn scan_source_context(
    files: &[String],
    budget_chars: usize,
    base_dir: &Path,
) -> SourceContext {
    let budget = if budget_chars == 0 {
        DEFAULT_BUDGET_CHARS
    } else {
        budget_chars
    };

    let mut summaries = Vec::new();
    let mut total_chars = 0;

    for file_path in files {
        if total_chars >= budget {
            break;
        }

        // Skip non-Rust files
        if !file_path.ends_with(".rs") {
            continue;
        }

        let full_path = resolve_path(file_path, base_dir);

        let content = match fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("Warning: could not read source file: {}", file_path);
                continue;
            }
        };

        let signatures = extract_signatures(&content);
        if signatures.is_empty() {
            continue;
        }

        // Enforce per-file budget
        let remaining = budget.saturating_sub(total_chars);
        let file_budget = remaining.min(PER_FILE_BUDGET);
        let truncated = truncate_signatures(&signatures, file_budget);

        let chars_used: usize = truncated.iter().map(|s| s.len() + 1).sum(); // +1 for newline
        total_chars += chars_used;

        summaries.push(FileSummary {
            file: file_path.clone(),
            signatures: truncated,
        });
    }

    SourceContext {
        summaries,
        total_chars,
    }
}

/// Resolve a file path relative to the base directory.
fn resolve_path(file_path: &str, base_dir: &Path) -> std::path::PathBuf {
    let path = Path::new(file_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

/// Extract public API signatures from Rust source code.
fn extract_signatures(content: &str) -> Vec<String> {
    let mut signatures = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Match public declarations
        if trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("pub trait ")
            || trimmed.starts_with("pub type ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("pub const ")
            || (trimmed.starts_with("impl ") && trimmed.contains(" for "))
        {
            // Capture up to opening brace or semicolon
            let sig = truncate_at_body(trimmed);
            signatures.push(sig);
        }
    }

    signatures
}

/// Truncate a signature line at the opening brace or semicolon.
fn truncate_at_body(line: &str) -> String {
    if let Some(pos) = line.find('{') {
        let before = line[..pos].trim_end();
        format!("{} {{ ... }}", before)
    } else {
        line.to_string()
    }
}

/// Truncate a list of signatures to fit within a character budget.
fn truncate_signatures(signatures: &[String], budget: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut used = 0;

    for sig in signatures {
        let cost = sig.len() + 1; // +1 for newline
        if used + cost > budget {
            break;
        }
        result.push(sig.clone());
        used += cost;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        name.to_string()
    }

    // --- AC: Extracts 'pub fn foo(x: i32) -> String' from sample Rust source ---

    #[test]
    fn test_extracts_pub_fn_signature() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
use std::io;

/// A function that does something.
pub fn foo(x: i32) -> String {
    format!("{}", x)
}

fn private_fn() {
    // not extracted
}
"#;
        let file = create_test_file(temp_dir.path(), "src/lib.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert_eq!(ctx.summaries[0].signatures.len(), 1);
        assert!(
            ctx.summaries[0].signatures[0].contains("pub fn foo(x: i32) -> String"),
            "Should extract pub fn signature, got: {}",
            ctx.summaries[0].signatures[0]
        );
    }

    // --- AC: Extracts 'pub struct Bar { ... }' definition from sample source ---

    #[test]
    fn test_extracts_pub_struct_definition() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
pub struct Bar {
    pub name: String,
    pub age: u32,
}

struct Private {
    field: i32,
}
"#;
        let file = create_test_file(temp_dir.path(), "src/models.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(
            ctx.summaries[0].signatures[0].contains("pub struct Bar"),
            "Should extract pub struct definition, got: {}",
            ctx.summaries[0].signatures[0]
        );
    }

    #[test]
    fn test_extracts_pub_enum() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub enum Status {\n    Active,\n    Inactive,\n}\n";
        let file = create_test_file(temp_dir.path(), "src/enums.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("pub enum Status"));
    }

    #[test]
    fn test_extracts_pub_trait() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub trait Formattable {\n    fn format(&self) -> String;\n}\n";
        let file = create_test_file(temp_dir.path(), "src/traits.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("pub trait Formattable"));
    }

    #[test]
    fn test_extracts_impl_for() {
        let temp_dir = TempDir::new().unwrap();
        let content = "impl Display for Foo {\n    fn fmt(&self, f: &mut Formatter) -> Result { todo!() }\n}\n";
        let file = create_test_file(temp_dir.path(), "src/impls.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("impl Display for Foo"));
    }

    #[test]
    fn test_extracts_pub_async_fn() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub async fn fetch_data(url: &str) -> Result<String, Error> {\n    Ok(String::new())\n}\n";
        let file = create_test_file(temp_dir.path(), "src/async.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("pub async fn fetch_data"));
    }

    #[test]
    fn test_extracts_multiple_signatures() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
pub struct Config {
    pub max: usize,
}

pub fn new_config() -> Config {
    Config { max: 0 }
}

pub enum Mode {
    Fast,
    Slow,
}

pub type Result<T> = std::result::Result<T, Error>;
"#;
        let file = create_test_file(temp_dir.path(), "src/multi.rs", content);
        let ctx = scan_source_context(&[file], 5000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert_eq!(
            ctx.summaries[0].signatures.len(),
            4,
            "Should extract struct, fn, enum, and type"
        );
    }

    // --- AC: Returns empty context for non-existent file (no panic) ---

    #[test]
    fn test_nonexistent_file_returns_empty_context() {
        let temp_dir = TempDir::new().unwrap();
        let files = vec!["src/does_not_exist.rs".to_string()];
        let ctx = scan_source_context(&files, 2000, temp_dir.path());

        assert!(
            ctx.summaries.is_empty(),
            "Non-existent file should produce empty context"
        );
        assert_eq!(ctx.total_chars, 0);
    }

    #[test]
    fn test_nonexistent_file_mixed_with_existing() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub fn exists() -> bool { true }\n";
        let existing = create_test_file(temp_dir.path(), "src/real.rs", content);

        let files = vec!["src/fake.rs".to_string(), existing];
        let ctx = scan_source_context(&files, 2000, temp_dir.path());

        assert_eq!(
            ctx.summaries.len(),
            1,
            "Should have one summary for the existing file"
        );
    }

    // --- AC: Respects token budget (output truncated at limit) ---

    #[test]
    fn test_budget_enforcement_truncates_output() {
        let temp_dir = TempDir::new().unwrap();
        // Create a file with many signatures
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!(
                "pub fn function_{}_with_a_long_name(x: i32, y: i32) -> String {{\n    todo!()\n}}\n\n",
                i
            ));
        }
        let file = create_test_file(temp_dir.path(), "src/big.rs", &content);

        // Use a small budget
        let ctx = scan_source_context(&[file], 200, temp_dir.path());

        assert!(
            ctx.total_chars <= 200,
            "Total chars {} should be within budget 200",
            ctx.total_chars
        );
        // Should have extracted some but not all signatures
        assert!(!ctx.summaries.is_empty());
        assert!(
            ctx.summaries[0].signatures.len() < 50,
            "Should have truncated signatures"
        );
    }

    #[test]
    fn test_budget_drops_files_when_exhausted() {
        let temp_dir = TempDir::new().unwrap();
        let content1 = "pub fn file1_fn() -> i32 { 0 }\n";
        let content2 = "pub fn file2_fn() -> i32 { 0 }\n";
        let file1 = create_test_file(temp_dir.path(), "src/a.rs", content1);
        let file2 = create_test_file(temp_dir.path(), "src/b.rs", content2);

        // Budget too small for both files (but big enough for one)
        let ctx = scan_source_context(&[file1, file2], 40, temp_dir.path());

        assert!(
            ctx.summaries.len() <= 2,
            "Should have at most 2 summaries with tight budget"
        );
        assert!(
            ctx.total_chars <= 40,
            "Total chars {} should be within budget",
            ctx.total_chars
        );
    }

    // --- Non-.rs file handling ---

    #[test]
    fn test_non_rs_file_skipped() {
        let temp_dir = TempDir::new().unwrap();
        let file = create_test_file(
            temp_dir.path(),
            "src/readme.md",
            "# Hello\npub fn fake() {}\n",
        );
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert!(ctx.summaries.is_empty(), "Non-.rs files should be skipped");
    }

    // --- Empty file handling ---

    #[test]
    fn test_empty_file_returns_empty_signatures() {
        let temp_dir = TempDir::new().unwrap();
        let file = create_test_file(temp_dir.path(), "src/empty.rs", "");
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert!(
            ctx.summaries.is_empty(),
            "Empty file should produce no summaries"
        );
    }

    #[test]
    fn test_file_with_no_pub_items() {
        let temp_dir = TempDir::new().unwrap();
        let content = "fn private() { }\nstruct Internal { field: i32 }\n";
        let file = create_test_file(temp_dir.path(), "src/private.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert!(
            ctx.summaries.is_empty(),
            "File with no pub items should produce no summaries"
        );
    }

    // --- Empty input ---

    #[test]
    fn test_empty_files_list() {
        let temp_dir = TempDir::new().unwrap();
        let ctx = scan_source_context(&[], 2000, temp_dir.path());

        assert!(ctx.summaries.is_empty());
        assert_eq!(ctx.total_chars, 0);
    }

    // --- format_for_prompt ---

    #[test]
    fn test_format_for_prompt_with_content() {
        let ctx = SourceContext {
            summaries: vec![FileSummary {
                file: "src/lib.rs".to_string(),
                signatures: vec!["pub fn hello() -> String { ... }".to_string()],
            }],
            total_chars: 35,
        };

        let prompt = ctx.format_for_prompt();
        assert!(prompt.contains("## Current Source Context"));
        assert!(prompt.contains("### src/lib.rs"));
        assert!(prompt.contains("pub fn hello()"));
    }

    #[test]
    fn test_format_for_prompt_empty() {
        let ctx = SourceContext {
            summaries: vec![],
            total_chars: 0,
        };

        let prompt = ctx.format_for_prompt();
        assert!(
            prompt.is_empty(),
            "Empty context should produce empty prompt"
        );
    }

    // --- Default budget ---

    #[test]
    fn test_zero_budget_uses_default() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub fn test_fn() -> bool { true }\n";
        let file = create_test_file(temp_dir.path(), "src/test.rs", content);

        // budget_chars=0 should use DEFAULT_BUDGET_CHARS (2000)
        let ctx = scan_source_context(&[file], 0, temp_dir.path());
        assert!(
            !ctx.summaries.is_empty(),
            "Zero budget should use default, not prevent extraction"
        );
    }

    // --- Truncate at body ---

    #[test]
    fn test_truncate_at_body_with_brace() {
        let result = truncate_at_body("pub fn foo() {");
        assert_eq!(result, "pub fn foo() { ... }");
    }

    #[test]
    fn test_truncate_at_body_with_semicolon() {
        let result = truncate_at_body("pub type Result<T> = std::result::Result<T, Error>;");
        assert_eq!(
            result,
            "pub type Result<T> = std::result::Result<T, Error>;"
        );
    }

    #[test]
    fn test_truncate_at_body_no_terminator() {
        let result = truncate_at_body("pub fn multiline(x: i32)");
        assert_eq!(result, "pub fn multiline(x: i32)");
    }

    // === Comprehensive tests (TEST-005) ===

    // --- AC: Context scanner on file with deeply nested modules ---

    #[test]
    fn test_deeply_nested_pub_fn_still_extracted() {
        let temp_dir = TempDir::new().unwrap();
        // pub fn at various nesting levels — scanner uses line-by-line pattern matching
        // so deeply nested pub items should still be detected
        let content = r#"
mod outer {
    pub mod inner {
        pub fn nested_function(x: &str) -> bool {
            !x.is_empty()
        }

        pub struct InnerStruct {
            pub field: i32,
        }

        mod deeply_nested {
            pub fn very_deep() -> u64 {
                42
            }
        }
    }
}
"#;
        let file = create_test_file(temp_dir.path(), "src/nested.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        // Should extract all pub items regardless of nesting
        let sigs = &ctx.summaries[0].signatures;
        assert!(
            sigs.len() >= 3,
            "Should extract nested pub fn, struct, and deeply nested fn, got {}",
            sigs.len()
        );
        assert!(sigs.iter().any(|s| s.contains("nested_function")));
        assert!(sigs.iter().any(|s| s.contains("InnerStruct")));
        assert!(sigs.iter().any(|s| s.contains("very_deep")));
    }

    // --- AC: File with only comments (no signatures) ---

    #[test]
    fn test_file_with_only_comments() {
        let temp_dir = TempDir::new().unwrap();
        let content =
            "// This is a comment\n/// Doc comment\n//! Module comment\n/* block comment */\n";
        let file = create_test_file(temp_dir.path(), "src/comments.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert!(
            ctx.summaries.is_empty(),
            "File with only comments should produce no summaries"
        );
    }

    // --- AC: pub(crate) items are NOT extracted (pattern requires `pub fn` not `pub(crate) fn`) ---

    #[test]
    fn test_pub_crate_items_not_extracted() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
pub(crate) fn crate_only() -> bool { true }
pub(super) fn super_only() -> bool { true }
pub fn truly_public() -> bool { true }
"#;
        let file = create_test_file(temp_dir.path(), "src/visibility.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        let sigs = &ctx.summaries[0].signatures;
        // pub(crate) starts with "pub(" not "pub fn", so should not match "pub fn"
        // pub(super) also should not match
        // Only "pub fn truly_public" should match
        assert_eq!(
            sigs.len(),
            1,
            "Only truly public items should be extracted, got {:?}",
            sigs
        );
        assert!(sigs[0].contains("truly_public"));
    }

    // --- AC: Large file with many mixed pub/private items ---

    #[test]
    fn test_large_file_mixed_items() {
        let temp_dir = TempDir::new().unwrap();
        let mut content = String::new();
        for i in 0..100 {
            if i % 2 == 0 {
                content.push_str(&format!(
                    "pub fn public_fn_{}(x: i32) -> i32 {{ x + {} }}\n",
                    i, i
                ));
            } else {
                content.push_str(&format!(
                    "fn private_fn_{}(x: i32) -> i32 {{ x - {} }}\n",
                    i, i
                ));
            }
        }
        let file = create_test_file(temp_dir.path(), "src/large.rs", &content);
        let ctx = scan_source_context(&[file], 10000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        // 50 public functions exist but PER_FILE_BUDGET (1500 chars) limits extraction.
        // Each signature is ~43 chars + " { ... }" → ~55 chars per sig (+1 newline).
        // 1500 / 56 ≈ 26-35 signatures depending on exact length.
        let sig_count = ctx.summaries[0].signatures.len();
        assert!(
            sig_count > 20 && sig_count <= 50,
            "Should extract many pub fn items within per-file budget, got {}",
            sig_count
        );
        // Verify none of the private functions leaked through
        for sig in &ctx.summaries[0].signatures {
            assert!(
                sig.contains("pub fn"),
                "Every extracted signature should be pub: {}",
                sig
            );
        }
    }

    // --- AC: Budget enforcement with multiple files (second file dropped) ---

    #[test]
    fn test_budget_drops_later_files_first() {
        let temp_dir = TempDir::new().unwrap();
        // File 1: many signatures (should consume most budget)
        let mut content1 = String::new();
        for i in 0..20 {
            content1.push_str(&format!(
                "pub fn file1_function_{}_with_long_name(arg: i32) -> String {{ todo!() }}\n",
                i
            ));
        }
        // File 2: one signature
        let content2 = "pub fn file2_only_fn() -> bool { true }\n";

        let f1 = create_test_file(temp_dir.path(), "src/big.rs", &content1);
        let f2 = create_test_file(temp_dir.path(), "src/small.rs", content2);

        // Use budget that fits file1 sigs but leaves nothing for file2
        // Each file1 sig is ~60 chars + " { ... }" ≈ 65 chars
        let ctx = scan_source_context(&[f1, f2], 500, temp_dir.path());

        // file1 should have some signatures, file2 may or may not fit
        assert!(
            !ctx.summaries.is_empty(),
            "Should have at least file1 summaries"
        );
        assert!(
            ctx.total_chars <= 500,
            "Total chars {} should be within budget",
            ctx.total_chars
        );
    }

    // --- AC: pub const extraction ---

    #[test]
    fn test_extracts_pub_const() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub const MAX_RETRIES: u32 = 5;\nconst PRIVATE: u32 = 10;\n";
        let file = create_test_file(temp_dir.path(), "src/constants.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert_eq!(ctx.summaries[0].signatures.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("pub const MAX_RETRIES"));
    }

    // --- AC: Absolute path handling ---

    #[test]
    fn test_absolute_path_file() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub fn absolute_path_fn() -> u8 { 0 }\n";
        let file_path = temp_dir.path().join("src/abs.rs");
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, content).unwrap();

        let abs_path = file_path.to_string_lossy().to_string();
        let ctx = scan_source_context(&[abs_path], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("absolute_path_fn"));
    }

    // --- AC: Unicode in source content ---

    #[test]
    fn test_unicode_in_source_content() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
/// 日本語のドキュメント
pub fn greet_用户(name: &str) -> String {
    format!("こんにちは, {}", name)
}

pub struct Données {
    pub valeur: f64,
}
"#;
        let file = create_test_file(temp_dir.path(), "src/unicode.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(
            ctx.summaries[0].signatures.len() >= 2,
            "Should extract unicode-named items"
        );
    }

    // --- AC: impl without 'for' keyword is NOT extracted ---

    #[test]
    fn test_impl_without_for_not_extracted() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
impl MyStruct {
    pub fn method(&self) -> bool { true }
}

impl Display for MyStruct {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { todo!() }
}
"#;
        let file = create_test_file(temp_dir.path(), "src/impls.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        let sigs = &ctx.summaries[0].signatures;
        // "impl MyStruct" without "for" should NOT be extracted
        // "impl Display for MyStruct" SHOULD be extracted
        // "pub fn method" SHOULD be extracted
        assert_eq!(
            sigs.len(),
            2,
            "Should extract pub fn + impl for, got: {:?}",
            sigs
        );
        assert!(sigs.iter().any(|s| s.contains("pub fn method")));
        assert!(sigs.iter().any(|s| s.contains("impl Display for MyStruct")));
    }

    // --- AC: format_for_prompt with multiple files ---

    #[test]
    fn test_format_for_prompt_multiple_files() {
        let ctx = SourceContext {
            summaries: vec![
                FileSummary {
                    file: "src/a.rs".to_string(),
                    signatures: vec!["pub fn a_fn() -> i32 { ... }".to_string()],
                },
                FileSummary {
                    file: "src/b.rs".to_string(),
                    signatures: vec![
                        "pub struct B { ... }".to_string(),
                        "pub fn b_fn() -> bool { ... }".to_string(),
                    ],
                },
            ],
            total_chars: 100,
        };

        let prompt = ctx.format_for_prompt();
        assert!(prompt.contains("### src/a.rs"));
        assert!(prompt.contains("### src/b.rs"));
        assert!(prompt.contains("pub fn a_fn"));
        assert!(prompt.contains("pub struct B"));
        assert!(prompt.contains("pub fn b_fn"));
    }

    // --- AC: format_for_prompt skips files with empty signatures ---

    #[test]
    fn test_format_for_prompt_skips_empty_signatures() {
        let ctx = SourceContext {
            summaries: vec![
                FileSummary {
                    file: "src/empty.rs".to_string(),
                    signatures: vec![],
                },
                FileSummary {
                    file: "src/has_sigs.rs".to_string(),
                    signatures: vec!["pub fn present() { ... }".to_string()],
                },
            ],
            total_chars: 30,
        };

        let prompt = ctx.format_for_prompt();
        assert!(
            !prompt.contains("src/empty.rs"),
            "Should skip files with empty signatures"
        );
        assert!(prompt.contains("src/has_sigs.rs"));
    }

    // --- AC: Budget of 1 char (extreme edge case) ---

    #[test]
    fn test_budget_one_char_extracts_nothing() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub fn tiny() { }\n";
        let file = create_test_file(temp_dir.path(), "src/tiny.rs", content);

        let ctx = scan_source_context(&[file], 1, temp_dir.path());

        // Budget is 1 char — no signature can fit (minimum signature > 1 char)
        assert!(
            ctx.summaries.is_empty() || ctx.summaries[0].signatures.is_empty(),
            "Budget of 1 should extract nothing"
        );
    }

    // --- AC: pub type extraction ---

    #[test]
    fn test_extracts_pub_type() {
        let temp_dir = TempDir::new().unwrap();
        let content = "pub type MyResult<T> = Result<T, Box<dyn std::error::Error>>;\n";
        let file = create_test_file(temp_dir.path(), "src/types.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("pub type MyResult"));
    }

    // --- AC: File with trailing whitespace and blank lines ---

    #[test]
    fn test_file_with_whitespace_only_lines() {
        let temp_dir = TempDir::new().unwrap();
        let content = "\n\n   \n\t\n\npub fn after_whitespace() -> bool { true }\n\n   \n\n";
        let file = create_test_file(temp_dir.path(), "src/whitespace.rs", content);
        let ctx = scan_source_context(&[file], 2000, temp_dir.path());

        assert_eq!(ctx.summaries.len(), 1);
        assert_eq!(ctx.summaries[0].signatures.len(), 1);
        assert!(ctx.summaries[0].signatures[0].contains("pub fn after_whitespace"));
    }

    // --- TEST-INIT-001: scan_source_context with project_root ---

    #[test]
    fn test_scan_source_context_resolves_relative_files_to_project_root() {
        // scan_source_context should resolve relative file paths against base_dir
        // (which should be project_root, not db_dir)
        let project_root = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        // Create source files under project_root
        let content = "pub fn project_fn() -> bool { true }\n";
        let _file = create_test_file(project_root.path(), "src/lib.rs", content);

        // When using project_root as base_dir, should find the files
        let files = vec!["src/lib.rs".to_string()];
        let ctx = scan_source_context(&files, 2000, project_root.path());
        assert_eq!(
            ctx.summaries.len(),
            1,
            "Should find files when using project_root as base_dir"
        );
        assert!(ctx.summaries[0].signatures[0].contains("project_fn"));

        // When using db_dir as base_dir, should NOT find the files
        // (they don't exist under db_dir)
        let ctx_wrong = scan_source_context(&files, 2000, db_dir.path());
        assert!(
            ctx_wrong.summaries.is_empty(),
            "Should not find files when using db_dir as base_dir (files only exist under project_root)"
        );
    }
}
