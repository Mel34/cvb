#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo test query_minus_terms_are_excluded
cargo test query_combines_or_required_and_excluded_terms
cargo test filter_matches_applies_query_to_record_content

printf 'Pull exclusion tests: OK\n'
