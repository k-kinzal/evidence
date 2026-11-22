//! Files the pages load: the wiki's own stylesheet and script, and the libraries for diagrams
//! and math. All are compiled into the binary and served from the wiki itself (`assets/…`),
//! never from another origin; `assets/vendor/SOURCES.md` records where each library came from.

use crate::markdown::Needs;

/// A file served under `assets/`.
#[derive(Debug, Clone, Copy)]
pub struct Asset {
    /// Path under `assets/`.
    pub path: &'static str,
    /// `Content-Type`.
    pub content_type: &'static str,
    /// Contents.
    pub bytes: &'static [u8],
    /// The path names a version, so the contents never change (cache for good).
    pub immutable: bool,
}

const IMMUTABLE: bool = true;
const FONT: &str = "font/woff2";

macro_rules! asset {
    ($path:literal, $type:expr, $immutable:expr) => {
        Asset {
            path: $path,
            content_type: $type,
            bytes: include_bytes!(concat!("../assets/", $path)),
            immutable: $immutable,
        }
    };
}

const JS: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";

/// `vendor/…` script paths.
const MERMAID: &str = "vendor/mermaid-11.4.1/mermaid.min.js";
const VIZ: &str = "vendor/viz-3.11.0/viz-standalone.js";
const VEGA: [&str; 4] = [
    "vendor/vega-5.30.0/vega.min.js",
    "vendor/vega-lite-5.21.0/vega-lite.min.js",
    // Evaluates Vega expressions without `new Function`, so the policy needs no `unsafe-eval`.
    "vendor/vega-interpreter-1.0.5/vega-interpreter.min.js",
    "vendor/vega-embed-6.26.0/vega-embed.min.js",
];
const KATEX: &str = "vendor/katex-0.16.11/katex.min.js";
const KATEX_CSS: &str = "vendor/katex-0.16.11/katex.min.css";

/// Every served file.
pub const ASSETS: &[Asset] = &[
    asset!("wiki.css", CSS, false),
    asset!("wiki.js", JS, false),
    asset!("vendor/mermaid-11.4.1/mermaid.min.js", JS, IMMUTABLE),
    asset!("vendor/viz-3.11.0/viz-standalone.js", JS, IMMUTABLE),
    asset!("vendor/vega-5.30.0/vega.min.js", JS, IMMUTABLE),
    asset!("vendor/vega-lite-5.21.0/vega-lite.min.js", JS, IMMUTABLE),
    asset!(
        "vendor/vega-interpreter-1.0.5/vega-interpreter.min.js",
        JS,
        IMMUTABLE
    ),
    asset!("vendor/vega-embed-6.26.0/vega-embed.min.js", JS, IMMUTABLE),
    asset!("vendor/katex-0.16.11/katex.min.js", JS, IMMUTABLE),
    asset!("vendor/katex-0.16.11/katex.min.css", CSS, IMMUTABLE),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_AMS-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Caligraphic-Bold.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Caligraphic-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Fraktur-Bold.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Fraktur-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Main-Bold.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Main-BoldItalic.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Main-Italic.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Main-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Math-BoldItalic.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Math-Italic.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_SansSerif-Bold.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_SansSerif-Italic.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_SansSerif-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Script-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Size1-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Size2-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Size3-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Size4-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
    asset!(
        "vendor/katex-0.16.11/fonts/KaTeX_Typewriter-Regular.woff2",
        FONT,
        IMMUTABLE
    ),
];

/// The asset at a path under `assets/`.
pub fn get(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

/// Scripts a page with these needs loads, in order.
pub fn scripts(needs: Needs) -> Vec<&'static str> {
    let mut out = Vec::new();
    if needs.mermaid {
        out.push(MERMAID);
    }
    if needs.graphviz {
        out.push(VIZ);
    }
    if needs.vega {
        out.extend(VEGA);
    }
    if needs.math {
        out.push(KATEX);
    }
    out
}

/// Stylesheets a page with these needs loads besides `wiki.css`.
pub fn styles(needs: Needs) -> Vec<&'static str> {
    if needs.math {
        vec![KATEX_CSS]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_referenced_file_is_bundled() {
        let all = Needs {
            mermaid: true,
            graphviz: true,
            vega: true,
            math: true,
        };
        for p in scripts(all).into_iter().chain(styles(all)) {
            assert!(get(p).is_some(), "{p}");
        }
        // Fonts the KaTeX stylesheet refers to.
        let css = std::str::from_utf8(get(KATEX_CSS).unwrap().bytes).unwrap();
        for part in css.split("url(").skip(1) {
            let rel = part.split(')').next().unwrap();
            assert!(
                get(&format!("vendor/katex-0.16.11/{rel}")).is_some(),
                "{rel}"
            );
        }
    }
}
