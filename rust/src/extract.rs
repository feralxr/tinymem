//! PDF text extraction: poppler's `pdftotext` when installed (best fidelity), otherwise a built-in parser.

use std::cell::Cell;
use std::path::Path;
use std::process::Command;

thread_local! {
    static IN_PDF: Cell<bool> = const { Cell::new(false) };
}

/// True while the current thread is inside the PDF parser (used to silence its panic messages).
pub fn in_pdf() -> bool {
    IN_PDF.with(|f| f.get())
}

fn pdftotext(path: &Path) -> Option<Vec<String>> {
    let out = Command::new("pdftotext")
        .args(["-layout", "-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut pages: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\u{c}')
        .map(str::to_string)
        .collect();
    if pages.last().map(|p| p.trim().is_empty()).unwrap_or(false) {
        pages.pop();
    }
    Some(pages)
}

pub fn pdf_pages(path: &Path) -> Result<Vec<String>, String> {
    if let Some(p) = pdftotext(path) {
        return Ok(p);
    }
    IN_PDF.with(|f| f.set(true));
    let p = path.to_path_buf();
    let res = std::panic::catch_unwind(move || pdf_extract::extract_text_by_pages(&p));
    IN_PDF.with(|f| f.set(false));
    match res {
        Ok(Ok(pages)) => Ok(pages),
        Ok(Err(e)) => Err(format!("could not read PDF: {}", e)),
        Err(_) => Err("could not read PDF: parser failed".to_string()),
    }
}
