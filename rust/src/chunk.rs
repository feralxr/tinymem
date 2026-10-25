//! Line-window chunker with blank-line-aware cuts (see docs/FORMAT.md).

pub const CHUNK_MAX_CHARS: usize = 1600;
pub const CHUNK_MAX_LINES: usize = 60;
pub const CHUNK_OVERLAP: usize = 3;
pub const LINE_CLIP: usize = 2000;

/// Returns `(start_line, end_line, text)` with 1-based inclusive line numbers.
pub fn chunk_lines(lines: &[String]) -> Vec<(usize, usize, String)> {
    let n = lines.len();
    let lens: Vec<usize> = lines.iter().map(|l| l.chars().count()).collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < n {
        let mut j = i;
        let mut size = 0usize;
        while j < n {
            let ln = lens[j] + 1;
            if j > i && (j - i >= CHUNK_MAX_LINES || size + ln > CHUNK_MAX_CHARS) {
                break;
            }
            size += ln;
            j += 1;
        }
        let mut end = j;
        if end < n {
            let lower = i + (end - i) * 2 / 5;
            let mut k = end - 1;
            while k > lower {
                if lines[k].trim().is_empty() {
                    end = k + 1;
                    break;
                }
                k -= 1;
            }
        }
        let text = lines[i..end].join("\n");
        if !text.trim().is_empty() {
            out.push((i + 1, end, text));
        }
        if end >= n {
            break;
        }
        i = if end > i + CHUNK_OVERLAP {
            end - CHUNK_OVERLAP
        } else {
            end
        };
    }
    out
}

pub fn split_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|l| {
            if l.len() > LINE_CLIP && l.chars().count() > LINE_CLIP {
                l.chars().take(LINE_CLIP).collect()
            } else {
                l.to_string()
            }
        })
        .collect()
}
