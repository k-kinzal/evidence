# Bundled libraries

These files are compiled into the `evidence-wiki` binary (`src/assets.rs`) and served by the wiki
itself under `assets/vendor/`, so no page loads code from another origin. Each package was taken
from its npm registry tarball after checking the tarball against the registry's `dist.integrity`.
Directory names carry the version, so a served file never changes under the same URL.

| Package | Version | License | Directory | From the package | Tarball integrity |
|---|---|---|---|---|---|
| `mermaid` | 11.4.1 | MIT | `mermaid-11.4.1/` | `mermaid.min.js` ← `dist/mermaid.min.js` | `sha512-Mb01JT/x6CKDWaxigwfZYuYmDZ6xtrNwNlidKZwkSrDaY9n90tdrJTV5Umk+wP1fZscGptmKFXHsXMDEVZ+Q6A==` |
| `@viz-js/viz` | 3.11.0 | MIT; contains Graphviz (EPL-1.0) and Expat (MIT) as object code | `viz-3.11.0/` | `viz-standalone.js` ← `lib/viz-standalone.js` | `sha512-3zoKLQUqShIhTPvBAIIgJUf5wO9aY0q+Ftzw1u26KkJX1OJjT7Z5VUqgML2GIzXJYFgjqS6a2VREMwrgChuubA==` |
| `vega` | 5.30.0 | BSD-3-Clause | `vega-5.30.0/` | `vega.min.js` ← `build/vega.min.js` | `sha512-ZGoC8LdfEUV0LlXIuz7hup9jxuQYhSaWek2M7r9dEHAPbPrzSQvKXZ0BbsJbrarM100TGRpTVN/l1AFxCwDkWw==` |
| `vega-lite` | 5.21.0 | BSD-3-Clause | `vega-lite-5.21.0/` | `vega-lite.min.js` ← `build/vega-lite.min.js` | `sha512-hNxM9nuMqpI1vkUOhEx6ewEf23WWLmJxSFJ4TA86AW43ixJyqcLV+iSCO0NipuVTE0rlDcc2e8joSewWyOlEwA==` |
| `vega-embed` | 6.26.0 | BSD-3-Clause | `vega-embed-6.26.0/` | `vega-embed.min.js` ← `build/vega-embed.min.js` | `sha512-AZCTdKHDAuhp6TFZRQOOs332tStCwZr/5e4uZMNEuJL69A57cT66NNZJdNiCP6u66REzIToYtMJhMTL9wl5B3A==` |
| `vega-interpreter` | 1.0.5 | BSD-3-Clause | `vega-interpreter-1.0.5/` | `vega-interpreter.min.js` ← `build/vega-interpreter.min.js` | `sha512-po6oTOmeQqr1tzTCdD15tYxAQLeUnOVirAysgVEemzl+vfmvcEP7jQmlc51jz0jMA+WsbmE6oJywisQPu/H0Bg==` |
| `katex` | 0.16.11 | MIT | `katex-0.16.11/` | `katex.min.js` ← `dist/katex.min.js`<br>`katex.min.css` ← `dist/katex.min.css (edited: woff2 sources only)`<br>`fonts/*.woff2` ← `dist/fonts/*.woff2` | `sha512-RQrI8rlHY92OLf3rho/Ts8i/XvjgguEjOkO1BEXcU3N8BqPpSzBNwV/G0Ukr+P/l3ivvJUE/Fa/CwbS6HesGNQ==` |

SHA-256 of each bundled file:

```
0cdd387c9590a1a9f9794560022dbb59654a7d86f187aa0c81495ad42d3a7308  katex-0.16.11/fonts/KaTeX_AMS-Regular.woff2
de7701e42cf1f4cf0b766c03fb27977207eee2f4fd5d76fa82188406da43ea4c  katex-0.16.11/fonts/KaTeX_Caligraphic-Bold.woff2
5d53e70ad607c2352162dec9e0923fb54ecdafaccbf604cd8dcf7d00facb989b  katex-0.16.11/fonts/KaTeX_Caligraphic-Regular.woff2
74444efd593c005e3f4573b44524704c0af0a937fe911cca9e94068d0d140d3f  katex-0.16.11/fonts/KaTeX_Fraktur-Bold.woff2
51814d270d06ff0255dba0799994fa4d8c84d11f09951d47595f4abb1f3602dc  katex-0.16.11/fonts/KaTeX_Fraktur-Regular.woff2
0f60d1b897938ec918c8ce073092411baf9438f6739465693ff18b0f9d20b021  katex-0.16.11/fonts/KaTeX_Main-Bold.woff2
99cd42a3c072d918f2f44984a807cf7aa16e13545fd0875fc07c6c65f99e715b  katex-0.16.11/fonts/KaTeX_Main-BoldItalic.woff2
97479ca6cce906abc961ecac96faa5f9ca2e61b8e7670d475826bcdee9a7c267  katex-0.16.11/fonts/KaTeX_Main-Italic.woff2
c2342cd8b869e01752a9321dc17213fc40d4d04c79688c1d43f2cf316abd7866  katex-0.16.11/fonts/KaTeX_Main-Regular.woff2
dc47344dbb6cb5b655c8460d561f4df5f501b90c804ad3c6cec65fe322351ab1  katex-0.16.11/fonts/KaTeX_Math-BoldItalic.woff2
7af58c5ec8f132a2ddde9027c6d7814decce4d3b822a11192a42a20e2e973264  katex-0.16.11/fonts/KaTeX_Math-Italic.woff2
e99ae51144bf1232efcc1bfe5add36262c6866b0faab24fa75740e1b98577a62  katex-0.16.11/fonts/KaTeX_SansSerif-Bold.woff2
00b26ac825e2095056396e0553b8ac26d3f8ad158c3826e28b4c45b385c4714a  katex-0.16.11/fonts/KaTeX_SansSerif-Italic.woff2
68e8c73ef42afd3ccec58bf0fba302cce448938e7fc020a5e31f8a952eee1342  katex-0.16.11/fonts/KaTeX_SansSerif-Regular.woff2
036d4e95149b69ff9bcc0cd55771efeb25ffa3947293e69acd78d5ac328c684b  katex-0.16.11/fonts/KaTeX_Script-Regular.woff2
6b47c40166b6dbe21a5dfca7718413f2147fd2399be1ba605d8ad39cedf25dfe  katex-0.16.11/fonts/KaTeX_Size1-Regular.woff2
d04c54219f9eaec6d4d4fd42dfb28785975a4794d6b2fc71e566b9cd6db842dd  katex-0.16.11/fonts/KaTeX_Size2-Regular.woff2
73d591271b1604960cb10bb90fee021670af7297017e0e98480b332d11f51995  katex-0.16.11/fonts/KaTeX_Size3-Regular.woff2
a4af7d414440a1c1790825cfb700cf9cf43b0f2c4b04f0ebc523011ad9853ec0  katex-0.16.11/fonts/KaTeX_Size4-Regular.woff2
71d517d67827787cfabdf186914cc3358eda539e37931941f2b2fd4a21f68c0b  katex-0.16.11/fonts/KaTeX_Typewriter-Regular.woff2
f0dbfcc2940b4d788c805c1a1e117e898d2814b0f1a52bf16640543216e0964d  katex-0.16.11/katex.min.css
e6bfe5deebd4c7ccd272055bab63bd3ab2c73b907b6e6a22d352740a81381fd4  katex-0.16.11/katex.min.js
a43bc1afd446f9c4cc66ac5dd45d02e8d65e26fc5344ec0ef787f88d6ddb6f9e  mermaid-11.4.1/mermaid.min.js
e432c751a6363f4a61da62920cc7d7ebd13cf09d82949f8f486248f8071dc3ce  vega-5.30.0/vega.min.js
072c054f2a6310725e038c38a71e00052705e31835632462c9717a23a384e895  vega-embed-6.26.0/vega-embed.min.js
53be81f6c7fb5d30aa32dc61bf938d37d43299f1ad9dd16957077fb01f72240a  vega-interpreter-1.0.5/vega-interpreter.min.js
cd32314b1e76e7d879dc9f0534b62be714df03554486c7ca2381abfd0a92d2f4  vega-lite-5.21.0/vega-lite.min.js
239ffd254812a48f15babf649770fabb6a7468a854cd877521bdb2ab936298f7  viz-3.11.0/viz-standalone.js
```

To update a library: download `https://registry.npmjs.org/<name>/<version>`, check the tarball
against `dist.integrity`, put the files in a new `<name>-<version>/` directory with the package's
license, update `src/assets.rs` and this file, and open a page that uses it with the browser
console open: the Content Security Policy reports anything it blocks.
