//! Test fixtures shared by the markdown module's test suites.

/// A short assistant answer with one of every common block: a heading, inline
/// code, a tight ordered list with bold, a fenced Rust block, a quote and a
/// closing paragraph.
///
/// Copied verbatim from letibot's `sessionlog::testing::MARKDOWN`, which is the
/// message its replay fixtures stream; the tests here were written against these
/// exact bytes, so they are kept byte for byte rather than paraphrased.
pub const MARKDOWN: &str = "\
## Why the cache missed

The short answer is `reasoning_content`. Three things had to line up:

1. The dialect replays prior reasoning into the field the model expects.
2. The ledger appends **ids**, never re-derived text.
3. The stable prefix is frozen before the first turn.

Here is the shape of the check:

```rust
assert_eq!(
    cached_tokens(n + 1),
    prompt_tokens(n) + committed_tokens(n),
);
```

> Note that `predicted_tokens` is the wrong term here — a trailing stop token is
> stripped before commit, so the witness must record **committed** tokens.

That is the whole of it. The remaining divergence is the server's own checkpoint
behaviour on a hybrid model, which is a measurement of llama.cpp and not a
violation of the invariant.
";
