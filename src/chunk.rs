//! 长 diff 的分块，以及**不依赖 LLM** 的本地结构化摘要。
//!
//! 本地摘要存在的意义：分块摘要那一步会调用模型，而模型可能因为 token 上限、
//! 网关行为等原因给不出内容。那种情况下不能整个工具作废——从 diff 本身提取
//! 文件名与增删规模是纯文本处理，永远可用。

/// 把长文本切成不超过 `chunk_size` 字节的块。
///
/// 优先在换行处断开；切点必须落在 UTF-8 字符边界上，否则 `&text[..]`
/// 会因为切断多字节字符（例如中文）而 panic。
pub fn split_into_chunks(text: &str, chunk_size: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let end_limit = std::cmp::min(start + chunk_size, text.len());
        // 必须先收回到字符边界：否则下一行按字节切片取窗口就会直接 panic
        let mut end = floor_char_boundary(text, end_limit);

        // 再退到窗口内最后一个换行符之后，尽量避免把一行代码切成两半
        if end < text.len() {
            if let Some(offset) = text[start..end].rfind('\n') {
                end = start + offset + 1;
            }
        }

        if end == start {
            // chunk_size 小于单个字符宽度时，至少前进一个完整字符
            end = (start + 1..=text.len())
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(text.len());
        }

        chunks.push(&text[start..end]);
        start = end;
    }

    chunks
}

/// 把索引向下取整到最近的 UTF-8 字符边界。
fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 一个文件的改动规模。
#[derive(Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

/// 从一段 diff 里提取出的结构化信息。
#[derive(Debug, Default)]
pub struct ChunkDigest {
    pub files: Vec<FileChange>,
    pub added: usize,
    pub removed: usize,
    /// 本块没有出现文件头，说明它是上一块某个文件的续段
    pub continuation_of: Option<String>,
}

impl ChunkDigest {
    /// 渲染成一行给人看的文本（不含「第几块」前缀，由调用方拼接）。
    pub fn render(&self) -> String {
        if let Some(path) = &self.continuation_of {
            return format!(
                "（该块为 {} 的续段，约 +{}/-{}）",
                path, self.added, self.removed
            );
        }
        if self.files.is_empty() {
            return "（本块没有可识别的文件变更）".to_string();
        }

        let detail: Vec<String> = self
            .files
            .iter()
            .map(|f| format!("{}(+{}/-{})", f.path, f.added, f.removed))
            .collect();
        format!(
            "文件: {}  合计 +{}/-{}",
            detail.join(", "),
            self.added,
            self.removed
        )
    }
}

/// 解析一段 unified diff，提取文件名与增删行数。
///
/// `last_file` 是上一块最后出现的文件名，用来标注续段——第 2..N 块常常从
/// 一个 hunk 中间开始，没有任何 `diff --git` / `+++` 头。
pub fn local_chunk_digest(chunk: &str, last_file: Option<&str>) -> ChunkDigest {
    let mut digest = ChunkDigest::default();
    let mut current: Option<FileChange> = None;

    for raw_line in chunk.lines() {
        // 兼容 CRLF
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if let Some(rest) = line.strip_prefix("diff --git ") {
            // 形如 `diff --git a/x b/y`，取 b/ 侧；没有 b/ 就退回 a/ 侧
            if let Some(change) = current.take() {
                digest.files.push(change);
            }
            let path = rest
                .split_once(" b/")
                .map(|(_, after)| after)
                .or_else(|| rest.split_once(" a/").map(|(_, after)| after))
                .unwrap_or(rest)
                .to_string();
            current = Some(FileChange {
                path,
                added: 0,
                removed: 0,
            });
            continue;
        }

        // `+++ b/x` 确认目标文件；`+++ /dev/null` 表示删除，沿用 diff --git 推出的路径
        if let Some(rest) = line.strip_prefix("+++ ") {
            if rest != "/dev/null" {
                let path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
                match current.as_mut() {
                    Some(change) => change.path = path,
                    None => {
                        current = Some(FileChange {
                            path,
                            added: 0,
                            removed: 0,
                        })
                    }
                }
            }
            continue;
        }

        // 二进制文件没有 +/- 行，但文件本身要计入
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            if current.is_none() {
                current = Some(FileChange {
                    path: "（二进制文件）".to_string(),
                    added: 0,
                    removed: 0,
                });
            }
            continue;
        }

        // 跳过 `--- a/x` 这类头，只统计真正的增删行
        if line.starts_with("--- ") || line.starts_with("@@") {
            continue;
        }

        if let Some(change) = current.as_mut() {
            if let Some(rest) = line.strip_prefix('+') {
                change.added += 1;
                digest.added += 1;
                let _ = rest;
            } else if let Some(rest) = line.strip_prefix('-') {
                change.removed += 1;
                digest.removed += 1;
                let _ = rest;
            }
        } else if line.starts_with('+') || line.starts_with('-') {
            // 没有文件头就出现了增删行 —— 典型的续段
            digest.added += usize::from(line.starts_with('+'));
            digest.removed += usize::from(line.starts_with('-'));
        }
    }

    if let Some(change) = current {
        digest.files.push(change);
    }

    if digest.files.is_empty() && (digest.added > 0 || digest.removed > 0) {
        digest.continuation_of = last_file.map(str::to_string);
    }

    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试：阈值落在多字节字符中间时不能 panic，也不能丢字符。
    /// 旧实现直接按字节切片，遇到中文 diff 会 panic。
    #[test]
    fn splits_on_char_boundaries() {
        let text = "修复分块处理逻辑".repeat(500);
        for chunk_size in [1, 2, 3, 4, 5, 7, 11, 13, 64] {
            let chunks = split_into_chunks(&text, chunk_size);
            assert!(
                chunks.iter().all(|c| !c.is_empty()),
                "产生空块: {}",
                chunk_size
            );
            assert_eq!(chunks.concat(), text, "内容不一致: {}", chunk_size);
        }
    }

    /// 优先在换行处断开，避免把一行代码切成两半。
    #[test]
    fn prefers_line_boundaries() {
        let text = "aaa\nbbbbbbbbbb\ncc\ndddddddddd\n";
        let chunks = split_into_chunks(text, 10);

        assert_eq!(chunks.concat(), text);
        for pair in chunks.windows(2) {
            assert!(
                pair[0].ends_with('\n') || pair[1].starts_with('\n'),
                "未在换行处断开: {:?} | {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    /// 超长单行（没有换行可退）也必须保证字符边界安全。
    #[test]
    fn handles_long_line_without_newline() {
        let text = "超长单行内容".repeat(100);
        let chunks = split_into_chunks(&text, 7);

        assert_eq!(chunks.concat(), text);
        assert!(chunks.iter().all(|c| !c.is_empty()));
    }

    #[test]
    fn empty_text_yields_no_chunks() {
        assert!(split_into_chunks("", 100).is_empty());
    }

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 context line
-removed one
+added one
+added two
diff --git a/src/new.rs b/src/new.rs
new file mode 100644
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,2 @@
+line one
+line two
";

    #[test]
    fn digest_extracts_files_and_counts() {
        let digest = local_chunk_digest(SAMPLE, None);

        assert_eq!(digest.files.len(), 2);
        assert_eq!(digest.files[0].path, "src/main.rs");
        assert_eq!((digest.files[0].added, digest.files[0].removed), (2, 1));
        // 新增文件用 `+++ /dev/null` 的反面表达，路径要从 diff --git 推出来
        assert_eq!(digest.files[1].path, "src/new.rs");
        assert_eq!((digest.files[1].added, digest.files[1].removed), (2, 0));
        assert_eq!((digest.added, digest.removed), (4, 1));
    }

    #[test]
    fn digest_render_mentions_files_and_totals() {
        let rendered = local_chunk_digest(SAMPLE, None).render();

        assert!(rendered.contains("src/main.rs(+2/-1)"), "{rendered}");
        assert!(rendered.contains("src/new.rs(+2/-0)"), "{rendered}");
        assert!(rendered.contains("合计 +4/-1"), "{rendered}");
    }

    /// 删除文件时 `+++` 是 /dev/null，路径不能变成 "/dev/null"
    #[test]
    fn digest_handles_deleted_file() {
        let chunk = "\
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-gone one
-gone two
";
        let digest = local_chunk_digest(chunk, None);

        assert_eq!(digest.files[0].path, "old.txt");
        assert_eq!(digest.removed, 2);
    }

    /// 二进制文件没有 +/- 行，但必须计入，否则整块会显示成「无变更」
    #[test]
    fn digest_handles_binary_file() {
        let chunk = "\
diff --git a/logo.png b/logo.png
index 111..222 100644
Binary files a/logo.png and b/logo.png differ
";
        let digest = local_chunk_digest(chunk, None);

        assert_eq!(digest.files.len(), 1);
        assert_eq!(digest.files[0].path, "logo.png");
    }

    /// 第 2..N 块常常从 hunk 中间开始，没有任何文件头
    #[test]
    fn digest_marks_continuation_chunk() {
        let chunk = "+又加了一行\n-又删了一行\n+再加一行\n";
        let digest = local_chunk_digest(chunk, Some("src/ai.rs"));

        assert_eq!(digest.continuation_of.as_deref(), Some("src/ai.rs"));
        assert_eq!((digest.added, digest.removed), (2, 1));
        assert!(
            digest.render().contains("src/ai.rs 的续段"),
            "{}",
            digest.render()
        );
    }

    /// CRLF 行尾不应影响统计
    #[test]
    fn digest_handles_crlf() {
        let chunk =
            "diff --git a/x b/x\r\n--- a/x\r\n+++ b/x\r\n@@ -1 +1,2 @@\r\n+added\r\n context\r\n";
        let digest = local_chunk_digest(chunk, None);

        assert_eq!(digest.files[0].path, "x");
        assert_eq!(digest.added, 1);
    }

    #[test]
    fn digest_on_empty_chunk_is_empty() {
        let digest = local_chunk_digest("", None);

        assert!(digest.files.is_empty());
        assert_eq!((digest.added, digest.removed), (0, 0));
    }
}
