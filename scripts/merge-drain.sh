#!/bin/sh
#
# Drain the merge queue: land every pull request that does not need Chris.
#
#     scripts/merge-drain.sh              # run until the queue is empty of unheld work
#     scripts/merge-drain.sh --once       # one pass, then exit (for a cron or a check)
#
# PROVISIONAL NAME. Minted 2026-08-04; not put to Chris. See the `Name:` block below.
#
# # Why this exists
#
# The maintainer holds merge authority, and merging is the one duty it is structurally worst at:
# when it is busy it is busy, and merging happens between conversations rather than during them. On
# 2026-08-04 that failed three ways in one evening. Two green pull requests sat unmerged for hours
# because nobody armed auto-merge. Merging one pull request staled the other eight under the
# repository's require-branches-to-be-up-to-date rule, and nothing picked them back up. And the
# steward, which exists precisely to compensate for the maintainer being busy, only ever *reported*
# a stalled queue; it never acted on one.
#
# So this is not a convenience. It is the same argument milestone 92 makes about audits: a practice
# that lives in someone's memory gets skipped exactly when it matters.
#
# # What it will not do
#
# **It never merges anything labelled `needs-chris`.** That label means the work is outside standing
# merge authority: it touches the syscall surface, adds a dependency, or owes a `DECISIONS` section.
# CLAUDE.md describes the label and the `## What I need from you` comment that must accompany it.
#
# It also stops rather than guessing. A conflict, or a failing check, ends the pass with a message
# naming the pull request, because both need a human decision and a loop that retries them just
# burns CI.
#
# # Why one at a time, which is the part that looks wrong
#
# Updating every stale branch at once would be faster in wall-clock terms and is the wrong thing.
# Each update triggers a full CI run, `cpu matrix` is this tree's load-sensitive check
# (notes/cpu-models.md), and eight concurrent QEMU-heavy runs manufacture their own failures. A
# serial drain is slower and does not invent work.
#
# Under the up-to-date rule this is inherently serial anyway: merging any pull request stales every
# other one, so the queue can only ever move one at a time.
#
# Name: unrecorded. Provisional, minted 2026-08-04 and not yet put to Chris. Named for what it does
# to the queue rather than for the mechanism, in the family of `qemu-bounded.sh`. It lives in
# `scripts/` rather than `script/` because it is a maintainer's tool and not a front door a
# contributor types; `script/` is the normalised "Scripts to Rule Them All" set (notes/scripts.md).
# See notes/merge-queue.md.

set -e
cd "$(dirname "$0")/.."

REPO="calef/cricker-os"
HELD_LABEL="needs-chris"
once=""
[ "$1" = "--once" ] && once=1

# The unheld queue, lowest number first. Drafts are excluded: a draft is not asking to be merged.
queue() {
	gh pr list --repo "$REPO" --state open \
		--json number,mergeStateStatus,labels,isDraft,title 2>/dev/null |
		jq -r --arg L "$HELD_LABEL" '
			[ .[]
			  | select(.isDraft == false)
			  | select((.labels | map(.name) | index($L)) | not) ]
			| sort_by(.number)' 2>/dev/null || echo '[]'
}

pass() {
	q=$(queue)
	n=$(printf '%s' "$q" | jq -r 'length' 2>/dev/null || echo 0)
	if [ "$n" = "0" ] || [ -z "$n" ]; then
		echo "merge-drain: queue empty; nothing open that does not need Chris"
		return 1
	fi

	num=$(printf '%s' "$q" | jq -r '.[0].number')
	state=$(printf '%s' "$q" | jq -r '.[0].mergeStateStatus')
	title=$(printf '%s' "$q" | jq -r '.[0].title')

	case "$state" in
	CLEAN | BEHIND | UNKNOWN)
		# Arm first, then update. Auto-merge fires on its own once the branch is current and green,
		# so the loop does not have to be alive at the moment it lands.
		gh pr merge "$num" --repo "$REPO" --auto --merge --delete-branch >/dev/null 2>&1 || true
		if [ "$state" = "BEHIND" ]; then
			echo "merge-drain: updating #$num against main ($title)"
			gh api -X PUT "repos/$REPO/pulls/$num/update-branch" >/dev/null 2>&1 || true
		else
			echo "merge-drain: #$num armed and current, waiting on checks ($title)"
		fi
		;;
	DIRTY)
		echo "merge-drain: STALLED. #$num has conflicts a person must resolve ($title)"
		return 1
		;;
	BLOCKED)
		failed=$(gh pr view "$num" --repo "$REPO" --json statusCheckRollup \
			-q '[.statusCheckRollup[] | select(.conclusion == "FAILURE") | .name] | join(", ")' 2>/dev/null)
		if [ -n "$failed" ]; then
			echo "merge-drain: STALLED. #$num is failing $failed ($title)"
			return 1
		fi
		echo "merge-drain: #$num waiting on checks ($title)"
		;;
	*)
		echo "merge-drain: #$num in state $state; leaving it alone ($title)"
		;;
	esac
	return 0
}

if [ -n "$once" ]; then
	pass || exit 0
	exit 0
fi

while pass; do
	sleep 150
done
