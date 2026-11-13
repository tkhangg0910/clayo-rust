//! Pages an agent writes (`<project>\.clayo\pages\*.html`): sealed with a Content-Security-Policy
//! before they are shown, and their folder kept out of git.

use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Where the shared page assets are served from in the C# viewer.
pub const ASSETS_URL: &str = "https://ccx.assets/pages/";

/// Scripts and styles from the page itself, Clayo's assets and three CDNs; images and fonts only
/// from inside the page; no connections and no form posts. A page can't send anything anywhere.
pub const CSP: &str = "default-src 'none'; \
    script-src 'unsafe-inline' https://ccx.assets https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com; \
    style-src 'unsafe-inline' https://ccx.assets https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com; \
    img-src data: blob: https://ccx.assets; \
    font-src data: https://ccx.assets; \
    connect-src 'none'; form-action 'none'";

/// [`CSP`] for a sealed copy opened in the default browser (PORTING.md D3): Clayo's assets sit
/// beside the file, so where the viewer allowed `https://ccx.assets` this allows the copy's own
/// folder. `'self'` is what a browser that gives `file:` pages an origin matches; `file:` covers
/// the ones that make every file its own opaque origin. Still no connections and no form posts.
pub const BROWSER_CSP: &str = "default-src 'none'; \
    script-src 'unsafe-inline' 'self' file: https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com; \
    style-src 'unsafe-inline' 'self' file: https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com; \
    img-src data: blob: 'self' file:; \
    font-src data: 'self' file:; \
    connect-src 'none'; form-action 'none'";

/// The asset files a page links to, beside it in a browser copy.
pub const ASSET_FILES: [&str; 4] = [
    "clayo-page.css",
    "clayo-page.js",
    "chart.umd.min.js",
    "mermaid.min.js",
];

static DOCTYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*<!doctype[^>]*>").expect("doctype regex"));

// href="clayo-page.css" and src="clayo-page.js", as the guide has them, with or without ./
// Both quotes are captured and must be the same (the regex crate has no backreferences).
static ASSET_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(href|src)(\s*=\s*)(["'])(?:\./)?(clayo-page\.(?:css|js))(["'])"#)
        .expect("asset link regex")
});

/// The page with the policy as the first thing in it, straight after the doctype, where the
/// browser makes it the first element of the head whatever the page's own markup says: a policy
/// put after a tag the page wrote could be commented out by the page. Links to Clayo's
/// stylesheet and script point at [`ASSETS_URL`].
pub fn seal(html: &str) -> String {
    seal_with(html, CSP, ASSETS_URL)
}

/// [`seal`] for a copy opened in the default browser, with Clayo's assets written beside it:
/// the asset links become `./clayo-page.css|js` and the policy is [`BROWSER_CSP`].
pub fn seal_for_browser(html: &str) -> String {
    seal_with(html, BROWSER_CSP, "./")
}

fn seal_with(html: &str, csp: &str, assets: &str) -> String {
    let meta = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">");
    let at = DOCTYPE.find(html).map_or(0, |m| m.end());
    let html = format!("{}{meta}{}", &html[..at], &html[at..]);
    ASSET_LINK
        .replace_all(&html, |c: &Captures| {
            if c[3] != c[5] {
                return c[0].to_string();
            }
            format!("{}{}{}{assets}{}{}", &c[1], &c[2], &c[3], &c[4], &c[3])
        })
        .into_owned()
}

const EXCLUDE_LINE: &str = ".clayo/";

/// Keeps a project's pages out of git without touching its .gitignore: adds `.clayo/` to the
/// repo's own `.git\info\exclude`, which is never committed. The session's folder may sit below
/// the repo's root. Does nothing outside a git repo, when the line is already there, or when the
/// file can't be read or written.
pub fn hide_from_git(project_folder: &Path) {
    let Some(root) = project_folder.ancestors().find(|d| d.join(".git").is_dir()) else {
        return;
    };
    let exclude = root.join(".git").join("info").join("exclude");
    let text = match std::fs::read_to_string(&exclude) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return,
    };
    if text.split('\n').any(|l| l.trim() == EXCLUDE_LINE) {
        return;
    }
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let _ = (|| -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(root.join(".git").join("info"))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude)?;
        writeln!(f, "{sep}{EXCLUDE_LINE}")
    })();
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = "<meta http-equiv=\"Content-Security-Policy\"";

    #[test]
    fn browser_copy_links_assets_beside_it() {
        let s = seal_for_browser(
            "<!doctype html><link href='clayo-page.css'><script src=\"./clayo-page.js\"></script>",
        );
        assert!(s.starts_with(&format!("<!doctype html>{META}")));
        assert!(s.contains("href='./clayo-page.css'"));
        assert!(s.contains("src=\"./clayo-page.js\""));
        assert!(!s.contains("ccx.assets"));
    }

    #[test]
    fn browser_policy_still_sends_nothing() {
        assert!(BROWSER_CSP.starts_with("default-src 'none'"));
        assert!(BROWSER_CSP.contains("connect-src 'none'"));
        assert!(BROWSER_CSP.contains("form-action 'none'"));
        assert!(BROWSER_CSP.contains("script-src 'unsafe-inline' 'self' file:"));
        assert!(!BROWSER_CSP.contains("ccx.assets"));
        let s = seal_for_browser("<p>");
        assert!(s.starts_with(&format!("{META} content=\"{BROWSER_CSP}\">")));
    }

    #[test]
    fn browser_copy_leaves_other_links_alone() {
        let other = "<script src=\"https://example.com/clayo-page.js\"></script>";
        assert!(seal_for_browser(other).ends_with(&format!(">{other}")));
    }

    #[test]
    fn mismatched_quotes_are_not_a_link() {
        let odd = "<link href=\"clayo-page.css'>";
        assert!(seal(odd).ends_with(&format!(">{odd}")));
    }
}
