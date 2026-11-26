#!/bin/sh
# usage: run_repo.sh <lang> <owner/repo>
lang=$1; repo=$2; slug=$(echo "$repo" | sed 's#/#__#')
mkdir -p ../out/per_repo/$lang
python3 -I mine_s1.py --lang "$lang" --repos "$repo" --cache ../data/api --out "../out/per_repo/$lang/$slug.jsonl" --stats "../out/per_repo/$lang/$slug.stats.json" 2>&1 | tail -2
