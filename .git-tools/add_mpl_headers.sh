#!/usr/bin/env bash
set -euo pipefail

header_rs='// Copyright 2025 Edgecast Cloud LLC.
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.'
header_hash='# Copyright 2025 Edgecast Cloud LLC.
# This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.'
header_html='<!--
Copyright 2025 Edgecast Cloud LLC.
This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
-->'

add_hash_header() {
  local f="$1"
  grep -q "Edgecast Cloud LLC." "$f" && return 0
  tmp=$(mktemp)
  { printf '%s\n\n' "$header_hash"; cat "$f"; } >"$tmp"
  mv "$tmp" "$f"
}

add_slash_header() {
  local f="$1"
  grep -q "Edgecast Cloud LLC." "$f" && return 0
  tmp=$(mktemp)
  { printf '%s\n\n' "$header_rs"; cat "$f"; } >"$tmp"
  mv "$tmp" "$f"
}

add_markdown_header() {
  local f="$1"
  grep -q "Edgecast Cloud LLC." "$f" && return 0
  tmp=$(mktemp)
  { printf '%s\n\n' "$header_html"; cat "$f"; } >"$tmp"
  mv "$tmp" "$f"
}

add_html_header() {
  local f="$1"
  grep -q "Edgecast Cloud LLC." "$f" && return 0
  # Insert after DOCTYPE if present, else at top
  if head -n1 "$f" | grep -qi "^<!DOCTYPE"; then
    tmp=$(mktemp)
    {
      head -n1 "$f"
      printf '%s\n' "$header_html"
      tail -n +2 "$f"
    } >"$tmp"
    mv "$tmp" "$f"
  else
    tmp=$(mktemp)
    { printf '%s\n' "$header_html"; cat "$f"; } >"$tmp"
    mv "$tmp" "$f"
  fi
}

should_skip() {
  local f="$1"
  case "$f" in
    Cargo.lock) return 0 ;;
    *.json) return 0 ;;
    openapi-specs/generated/*) return 0 ;;
  esac
  return 1
}

while IFS= read -r f; do
  if should_skip "$f"; then
    continue
  fi
  case "$f" in
    *.rs) add_slash_header "$f" ;;
    *.toml) add_hash_header "$f" ;;
    Makefile) add_hash_header "$f" ;;
    *.md) add_markdown_header "$f" ;;
    *.html) add_html_header "$f" ;;
    *) : ;; # ignore other types
  esac
done < <(git ls-files)

