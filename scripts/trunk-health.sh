#!/bin/sh
#
# Say when `main` is red, and say when it recovers.
#
#     scripts/trunk-health.sh             # watch until stopped
#     scripts/trunk-health.sh --once      # print the current state and exit
#
# PROVISIONAL NAME. Minted 2026-08-04; not put to calef. See the `Name:` block below.
#
# # Why this exists
#
# Nothing owned trunk health, and the gap was in the role definitions rather than in anyone's
# execution. A developer works one milestone in one lane and cannot see `main` by design. The
# steward watches pull request checks, conflicts, at-risk work and idle lanes, and its charter never
# mentioned the trunk. The maintainer is told to keep hygiene, and that list is prune the worktree,
# delete the branch, relink `nife-dev`, leave no QEMU. So the role that merges is the role that
# breaks `main`, and the role that exists to compensate for the merger being busy was not pointed at
# the thing merging breaks.
#
# **The signal was never missing.** CI runs on every push to `main` and has all along. On 2026-08-04
# `main` went red and was found by someone running `script/lint` by hand after a merge, which is luck
# rather than process. This reads the signal that already exists.
#
# # What it says, and what it deliberately does not
#
# It reports the transition to red, naming the failing workflows, and the transition back to green.
# It does **not** report every red poll, because a trunk that stays broken for an hour is one fact
# and not twenty-four.
#
# It says "nobody is assigned to this" on purpose. A red trunk with an owner is a task; a red trunk
# without one is the failure this script exists to surface, and the wording is the difference.
#
# **Recovery is reported too, deliberately.** A watcher that only speaks on failure trains its reader
# to treat silence as health, and silence is also what a dead watcher produces.
#
# # The thing that would prevent this rather than detect it
#
# GitHub's require-branches-to-be-up-to-date rule, applied 2026-08-04 (§73). Two pull requests, each
# green against the base it was cut from, merged in an order neither had ever been tested in and put
# `main` red. That rule forces a re-run against the new `main`, turning that failure into one re-run
# instead of a broken trunk. This script is the detection half; the rule is the prevention half.
#
# Name: unrecorded. Provisional, minted 2026-08-04 and not yet put to calef. `trunk` rather than
# `main` because the branch could be renamed and the concept could not, and because "trunk health"
# is the term the field already uses. See notes/merge-queue.md.

set -e
cd "$(dirname "$0")/.."

REPO="calef/nife"
once=""
[ "$1" = "--once" ] && once=1

state() {
	sha=$(git ls-remote "$(git remote get-url origin 2>/dev/null || echo origin)" refs/heads/main 2>/dev/null | cut -c1-8)
	[ -z "$sha" ] && { echo "unknown"; return; }
	runs=$(gh run list --repo "$REPO" --branch main --limit 12 \
		--json workflowName,status,conclusion,headSha 2>/dev/null || echo '[]')
	failed=$(printf '%s' "$runs" | jq -r --arg s "$sha" \
		'[.[] | select(.headSha[0:8] == $s) | select(.conclusion == "failure") | .workflowName] | unique | join(", ")' 2>/dev/null)
	running=$(printf '%s' "$runs" | jq -r --arg s "$sha" \
		'[.[] | select(.headSha[0:8] == $s) | select(.status != "completed")] | length' 2>/dev/null)
	if [ -n "$failed" ]; then
		echo "RED $sha $failed"
	elif [ "$running" = "0" ]; then
		echo "GREEN $sha"
	else
		echo "PENDING $sha"
	fi
}

if [ -n "$once" ]; then
	s=$(state)
	case "$s" in
	RED*) echo "main is RED: ${s#RED }" ;;
	GREEN*) echo "main is green at ${s#GREEN }" ;;
	*) echo "main: ${s}" ;;
	esac
	exit 0
fi

prev=""
while true; do
	s=$(state)
	case "$s" in
	RED*)
		[ "$s" != "$prev" ] && echo "MAIN IS RED at $(echo "$s" | cut -d' ' -f2) -- failing: $(echo "$s" | cut -d' ' -f3-) -- nobody is assigned to this"
		;;
	GREEN*)
		case "$prev" in
		RED*) echo "main recovered at $(echo "$s" | cut -d' ' -f2)" ;;
		esac
		;;
	esac
	prev="$s"
	sleep 90
done
