//! **The page cache: the last few pages, explicitly, in one place.**
//!
//! Three entries, least-recently-used evicted, keyed by page and dpi (a text page has no dpi and
//! says so with `dpi: 0`). Small on purpose: a page is a rasterized picture of a couple of
//! megabytes, and the point of a reader is that moving back one page is instant while the rest of
//! the document stays unrendered.
//!
//! **What it can and cannot do today.** The ABI instantiates a fresh module for every call
//! (`plans/S10-wasm-hooks.md` §2.1), so this cache is empty at the start of each call: within one
//! call it saves the repeat render that the picture-size ladder would otherwise pay, and nothing
//! more. The moment a host keeps one instance across a view's calls — which is the change §2.1
//! reserves for v2 — this is already the reader's page cache and no other line moves. That is why
//! it is a `static` with a fixed capacity rather than a local: the cost of it being useless today
//! is one mutex that never contends, and the cost of it being absent later is a re-render per key
//! press.

use std::sync::Mutex;

/// How many pages are kept. Three is what a reader needs: the page, the one behind it (going back
/// is a look, not a re-render) and one ahead.
pub const PAGES: usize = 3;

/// What a cached entry is keyed by. `text` rather than an enum so a key is one comparable value.
///
/// **`doc` is part of the key**, a hash of the document's path: a cache that keyed only on the page
/// number would show one document's page 3 in another document the moment a host keeps an instance
/// across two views, and "the page cache is small and explicit" must not mean "and wrong".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub doc: u64,
    pub page: u32,
    /// The dpi a picture was rendered at; `0` for a page's text, which has no resolution.
    pub dpi: u32,
    pub text: bool,
}

impl Key {
    /// A page's text.
    pub fn text(doc: u64, page: u32) -> Key {
        Key {
            doc,
            page,
            dpi: 0,
            text: true,
        }
    }

    /// A page's picture, rendered at `dpi`.
    pub fn picture(doc: u64, page: u32, dpi: u32) -> Key {
        Key {
            doc,
            page,
            dpi,
            text: false,
        }
    }
}

/// One cached page: its text, or the PNG bytes the host answered for it. The PNG is what a render
/// costs and what a decode reads, so it is what is kept; it is also a tenth of the RGB it becomes.
#[derive(Clone, Debug)]
pub enum Page {
    Text(String),
    Png(Vec<u8>),
}

/// The cache itself: most recently used first.
#[derive(Debug)]
pub struct Cache {
    cap: usize,
    slots: Vec<(Key, Page)>,
}

impl Cache {
    pub const fn new(cap: usize) -> Cache {
        Cache {
            cap,
            slots: Vec::new(),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The keys held, most recently used first — what a test reads to pin the order.
    pub fn keys(&self) -> Vec<Key> {
        self.slots.iter().map(|(k, _)| *k).collect()
    }

    /// **A hit, which also makes the entry the most recent.** `None` for a miss.
    pub fn get(&mut self, key: Key) -> Option<&Page> {
        let at = self.slots.iter().position(|(k, _)| *k == key)?;
        let slot = self.slots.remove(at);
        self.slots.insert(0, slot);
        self.slots.first().map(|(_, page)| page)
    }

    /// **A page, whether or not it was already there.** Evicts the least recently used.
    pub fn put(&mut self, key: Key, page: Page) {
        self.slots.retain(|(k, _)| *k != key);
        self.slots.insert(0, (key, page));
        self.slots.truncate(self.cap);
    }
}

/// **A document's identity in the cache**: FNV-1a over the path, because a key is compared and
/// copied on every lookup and a path is a `String` that would have to be cloned into every entry.
pub fn id(path: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// The one cache this module has.
static CACHE: Mutex<Cache> = Mutex::new(Cache::new(PAGES));

/// **Empty the cache.** A host that reloads a plugin, or opens a different document in the same
/// instance, must not read the last one's pages; today the fresh instance per call does this for
/// free, and this is what makes that explicit rather than incidental.
pub fn clear() {
    with(|c| {
        c.slots.clear();
    });
}

/// **Run `f` with the cache.** A poisoned lock is not a reason to lose a page: the entries are
/// plain values, so the guard is taken back out of the poison and the call goes on. (A panic in a
/// plugin is a trap the host reports; the cache must not be the second thing that goes wrong.)
pub fn with<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}
