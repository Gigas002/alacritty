#!/usr/bin/env bash
# Rebases the current branch (feature/kitty-protocol-graphics) onto upstream alacritty's master.
#
#   bump-upstream.sh check   prints changed=true|false (GitHub output format): is upstream ahead?
#   bump-upstream.sh rebase  rebases, resolving Cargo.lock conflicts, then runs verify. Leaves the
#                            result in HEAD; pushing is the caller's job.
#   bump-upstream.sh verify  commits Cargo.lock if cargo changes it, and checks that the vendored
#                            vte patch is still used.
#
# Cargo.lock is the one file both sides edit routinely (upstream's dependency bumps, the feature's
# png/miniz_oxide and vendored vte). Its conflicts are resolved by taking upstream's lock file and
# letting cargo add the feature's entries back. Any other conflict stops the rebase with the
# files listed. If upstream moves to a vte release the vendored copy no longer matches, cargo
# silently drops the patch and the graphics protocol stops working, so that is an error too.
set -euo pipefail

UPSTREAM_URL=${UPSTREAM_URL:-https://github.com/alacritty/alacritty.git}
UPSTREAM_BRANCH=${UPSTREAM_BRANCH:-master}

fetch() {
    git remote get-url upstream >/dev/null 2>&1 || git remote add upstream "$UPSTREAM_URL"
    git fetch --quiet upstream "$UPSTREAM_BRANCH"
}

check() {
    fetch
    if git merge-base --is-ancestor "upstream/$UPSTREAM_BRANCH" HEAD; then
        echo "changed=false"
    else
        echo "changed=true"
        echo "Upstream is $(git rev-list --count "HEAD..upstream/$UPSTREAM_BRANCH") commits ahead" >&2
    fi
}

resolve_lock() {
    # During a rebase "ours" is the commit being rebased onto: upstream's lock file.
    git checkout --ours -- Cargo.lock
    cargo metadata --format-version 1 >/dev/null
    git add Cargo.lock
}

rebase() {
    fetch
    if git rebase "upstream/$UPSTREAM_BRANCH"; then :; else
        while [ -d "$(git rev-parse --git-path rebase-merge)" ] || [ -d "$(git rev-parse --git-path rebase-apply)" ]; do
            conflicts=$(git diff --name-only --diff-filter=U)
            if [ "$conflicts" != "Cargo.lock" ]; then
                echo "::error::Rebase conflict in: $(echo $conflicts)" >&2
                echo "Conflicts at $(git log -1 --format='%h %s' REBASE_HEAD 2>/dev/null || echo '?'):" >&2
                echo "$conflicts" >&2
                git rebase --abort
                exit 1
            fi
            echo "Resolving Cargo.lock conflict" >&2
            resolve_lock
            GIT_EDITOR=true git rebase --continue && break || true
        done
    fi

    verify
    echo "Rebased onto upstream/$UPSTREAM_BRANCH $(git rev-parse --short "upstream/$UPSTREAM_BRANCH")" >&2
}

verify() {
    # A lock file that merged cleanly can still be stale (`--locked` misses dropped entries).
    cargo metadata --format-version 1 >/dev/null
    if ! git diff --quiet -- Cargo.lock; then
        echo "Updating Cargo.lock after the rebase" >&2
        git commit --quiet -m "chore: update Cargo.lock after rebasing onto upstream" -- Cargo.lock
    fi

    unused=$(cargo metadata --format-version 1 2>&1 >/dev/null | grep -i "patch .*vte.* was not used" || true)
    sources=$(cargo metadata --format-version 1 2>/dev/null \
        | python3 -c 'import json,sys; print(" ".join(sorted({str(p["source"]) for p in json.load(sys.stdin)["packages"] if p["name"] == "vte"})))')
    if [ -n "$unused" ] || [ "$sources" != "None" ]; then
        echo "::error::The vendored vte patch is no longer used (vte sources: $sources). Upstream likely moved to a newer vte: update vendor/vte to it, keeping the APC dispatch patch." >&2
        exit 1
    fi
}

"${1:?usage: bump-upstream.sh check|rebase|verify}"
