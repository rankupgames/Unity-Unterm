#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
notice="../Packages/dev.tnayuki.unterm/Third Party Notices.md"
cargo about generate about.hbs --fail -o "$notice"

# Some upstream license bodies use CRLF or retain trailing spaces. Normalize the
# generated artifact so Git diffs and Unity packages remain deterministic.
perl -pi -e 's/\r$//; s/[ \t]+$//' "$notice"
perl -0777 -pi -e 's/\n+\z/\n/' "$notice"
