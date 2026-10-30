#!/usr/bin/env bash
# Lists the images in issues and PRs closed since tag PREV (opening post and comments), as
# Markdown context for `make release-notes`. Prints nothing when `gh` is missing or not logged in.
#
#   scripts/release-images.sh 0.1.6
set -euo pipefail

prev="${1:?usage: release-images.sh <previous tag>}"
command -v gh >/dev/null && command -v jq >/dev/null || exit 0
since=$(git log -1 --format=%cI "$prev")
maintainer=$(gh api user -q .login 2>/dev/null) || exit 0

images='[scan("!\\[[^]]*\\]\\([^)]+\\)|<img[^>]+>")]'
for kind in issue pr; do
  gh "$kind" list --state closed --search "closed:>=$since" --limit 100 \
    --json number,title,author,body,comments,url
done | jq -rs --arg maintainer "$maintainer" "
  add | sort_by(.number)[]
  | {number, title, url,
     posts: ([{who: .author.login, where: \"opening post\", imgs: (.body // \"\" | $images)}]
             + [.comments[] | {who: .author.login, where: \"comment\", imgs: (.body | $images)}])
            | map(select(.imgs | length > 0))}
  | select(.posts | length > 0)
  | \"### #\(.number) \(.title) (\(.url))\",
    (.posts[] | \"- \(.where) by @\(.who)\(if .who == \$maintainer then \" (maintainer)\" else \"\" end):\",
                (.imgs[] | \"  \(.)\")),
    \"\"
"
