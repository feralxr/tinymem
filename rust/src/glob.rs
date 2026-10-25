//! fnmatch-style glob matching: `*`, `?`, `[set]`, `[!set]`, `[^set]`, ranges.

pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star_p = Some(pi);
                    star_t = ti;
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, next)) = class_match(&p, pi, t[ti]) {
                        if matched {
                            pi = next;
                            ti += 1;
                            continue;
                        }
                    } else if t[ti] == '[' {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c => {
                    if c == t[ti] {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
            }
        }
        match star_p {
            Some(sp) => {
                star_t += 1;
                ti = star_t;
                pi = sp + 1;
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Parse a `[...]` class starting at `p[start]`. Returns (matches, index after class)
/// or None when the class is unterminated (the `[` is then a literal).
fn class_match(p: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let mut negate = false;
    if i < p.len() && (p[i] == '!' || p[i] == '^') {
        negate = true;
        i += 1;
    }
    let first = i;
    let mut matched = false;
    while i < p.len() {
        if p[i] == ']' && i > first {
            return Some((matched != negate, i + 1));
        }
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if p[i] <= c && c <= p[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if p[i] == c {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::glob_match as g;

    #[test]
    fn basics() {
        assert!(g("*.py", "a/b/c.py"));
        assert!(g("src/*", "src/a/b.rs"));
        assert!(g("*auth*", "src/auth/login.py"));
        assert!(!g("*.py", "a.rs"));
        assert!(g("a?c", "abc"));
        assert!(g("[a-c]x", "bx"));
        assert!(!g("[!a-c]x", "bx"));
        assert!(g("[^a-c]x", "dx"));
        assert!(g("a[", "a["));
        assert!(g("", ""));
        assert!(!g("", "a"));
    }
}
