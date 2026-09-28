# canary-narrow-bug-fix

Fix `src/slug.py` so `slugify("Hello, World!")` returns `hello-world` and
whitespace is collapsed. The acceptance gate is `.canary/gate.sh`; the only
permitted repository mutation is `src/slug.py`.
