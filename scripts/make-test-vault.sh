#!/usr/bin/env bash
# Fills a test vault with real, permissively licensed markdown: the documentation folders of a
# few open-source projects (about 5,000 notes). Only the docs folders are fetched (sparse,
# shallow clones), only *.md files are copied, into <vault>/Docs/<project>/.
#
#   bash scripts/make-test-vault.sh <vault>
#
# Sources (licence):
#   duckdb/duckdb-web            docs/              MIT
#   dbt-labs/docs.getdbt.com     website/docs       Apache-2.0
#   kubernetes/website           content/en/docs    CC BY 4.0
#   microsoft/vscode-docs        docs/              CC BY 3.0
#   rust-lang/book               src/               MIT / Apache-2.0
set -euo pipefail

VAULT="${1:?usage: make-test-vault.sh <vault>}"
test -d "$VAULT/.obsidian" || { echo "$VAULT has no .obsidian folder"; exit 1; }
DOCS="$VAULT/Docs"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() { # name url path licence
  local name="$1" url="$2" path="$3" licence="$4"
  echo "== $name ($path)"
  git clone --quiet --depth 1 --filter=blob:none --sparse "$url" "$TMP/$name"
  git -C "$TMP/$name" sparse-checkout set --no-cone "$path" >/dev/null
  mkdir -p "$DOCS/$name"
  rsync -a --prune-empty-dirs --include '*/' --include '*.md' --exclude '*' "$TMP/$name/$path/" "$DOCS/$name/"
  printf '# %s\n\nMarkdown copied from <%s> (`%s`), licence: %s. Test data for Fast Find 10K only.\n' "$name" "$url" "$path" "$licence" > "$DOCS/$name/_source.md"
  echo "   $(find "$DOCS/$name" -name '*.md' | wc -l) notes"
  rm -rf "$TMP/$name"
}

fetch duckdb     https://github.com/duckdb/duckdb-web.git          docs            "MIT"
fetch dbt        https://github.com/dbt-labs/docs.getdbt.com.git   website/docs    "Apache-2.0"
fetch kubernetes https://github.com/kubernetes/website.git         content/en/docs "CC BY 4.0"
fetch vscode     https://github.com/microsoft/vscode-docs.git      docs            "CC BY 3.0"
fetch rust-book  https://github.com/rust-lang/book.git             src             "MIT / Apache-2.0"

echo "== total markdown notes in $VAULT: $(find "$VAULT" -name '*.md' -not -path '*/.obsidian/*' -not -path '*/.smart-env/*' -not -path '*/.trash/*' | wc -l)"
