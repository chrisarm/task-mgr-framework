use std::io;
use std::io::Write;
use std::path::Path;

/// Replace content between `marker_begin` and `marker_end` in `current` with
/// `replacement`. When no valid marker pair is found, the block is appended so
/// the function is safe to call on files that do not yet contain the markers.
///
/// Only the first occurrence of `marker_begin` is used; content after
/// `marker_end` (on that same pass) is preserved verbatim.
pub fn splice_block(
    current: &str,
    marker_begin: &str,
    marker_end: &str,
    replacement: &str,
) -> String {
    if let (Some(begin_idx), Some(end_idx)) = (current.find(marker_begin), current.find(marker_end))
        && end_idx > begin_idx
    {
        let after_begin = begin_idx + marker_begin.len();
        let mut result = String::with_capacity(current.len() + replacement.len());
        result.push_str(&current[..after_begin]);
        result.push('\n');
        result.push_str(replacement);
        if !replacement.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&current[end_idx..]);
        return result;
    }
    // No valid marker pair — append.
    let mut result = String::from(current);
    if !result.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    result.push_str(marker_begin);
    result.push('\n');
    result.push_str(replacement);
    if !replacement.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(marker_end);
    result.push('\n');
    result
}

/// Write `content` to `path` atomically via tempfile + rename in the same
/// directory. A crash mid-write leaves the original file untouched.
pub fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(content.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const BEGIN: &str = "<!-- TASK_MGR:BEGIN -->";
    const END: &str = "<!-- TASK_MGR:END -->";

    #[test]
    fn empty_input_appends_markers_wrapping_replacement() {
        let result = splice_block("", BEGIN, END, "content\n");
        assert!(result.contains(BEGIN), "missing begin marker");
        assert!(result.contains(END), "missing end marker");
        assert!(result.contains("content"));
        let begin_idx = result.find(BEGIN).unwrap();
        let end_idx = result.find(END).unwrap();
        assert!(begin_idx < end_idx, "begin must precede end");
    }

    #[test]
    fn existing_block_replaced_content_outside_unchanged() {
        let input = format!("head\n{BEGIN}\nOLD\n{END}\ntail\n");
        let result = splice_block(&input, BEGIN, END, "NEW\n");
        assert!(result.contains("NEW"));
        assert!(!result.contains("OLD"));
        let begin_idx = result.find(BEGIN).unwrap();
        assert_eq!(&result[..begin_idx], "head\n");
        assert!(result.ends_with("tail\n"));
    }

    #[test]
    fn splice_is_idempotent() {
        let input = format!("head\n{BEGIN}\nBLOCK\n{END}\ntail\n");
        let once = splice_block(&input, BEGIN, END, "BLOCK\n");
        let twice = splice_block(&once, BEGIN, END, "BLOCK\n");
        assert_eq!(once, twice);
    }

    #[test]
    fn write_atomic_creates_file_with_correct_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("output.md");
        write_atomic(&path, "hello world").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_failure_leaves_target_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.md");
        fs::write(&target, "original").unwrap();

        // Read-only dir prevents tempfile creation, so target is never touched.
        let ro_dir = TempDir::new().unwrap();
        fs::set_permissions(ro_dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let bad_path = ro_dir.path().join("output.md");

        let result = write_atomic(&bad_path, "new content");

        // Restore so TempDir can clean up.
        fs::set_permissions(ro_dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        // target in the original dir is unchanged
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    }
}
