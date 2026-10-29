# Working in this repository

## Merge gate

The merge queue runs these in a branch's worktree, after rebasing it onto `master` and before landing it. The first that fails parks the branch. Read from `master`'s copy of this file, so a branch cannot change its own gate.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Each needs a real memory allowance. Under a 64 MiB cap the compiler is killed and the failure reads like a build error.

CI runs more than this, including a release build and `scripts/check-dist-names.sh`. The gate is deliberately the fast subset: it catches everything a code change breaks, and the rest runs on push.
