#!/bin/sh
#
# Drain the merge queue: land every pull request that does not need calef.
#
#     scripts/merge-drain.sh              # run until the queue is empty of unheld work
#     scripts/merge-drain.sh --once       # one pass, then exit (for a cron or a check)
#
# PROVISIONAL NAME. Minted 2026-08-04; not put to calef. See the `Name:` block below.
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
# **It never merges anything labelled `needs-architect`.** That label means the work is outside standing
# merge authority: it touches the syscall surface, adds a dependency, or owes a `DECISIONS` section.
# CLAUDE.md describes the label and the `## What I need from you` comment that must accompany it.
#
# It also stops rather than guessing. A conflict, or a failing check, ends the pass with a message
# naming the pull request, because both need a human decision and a loop that retries them just
# burns CI.
#
# # Arming is breadth-first; updating is one at a time
#
# These two are not the same operation and the first version of this script treated them as one,
# which is the bug worth recording here rather than in a commit nobody re-reads. **Arming auto-merge
# is free**: it is one API call and it changes nothing until the checks pass. **Updating a branch is
# expensive**: it triggers a full CI run, `cpu matrix` is this tree's load-sensitive check
# (notes/cpu-models.md), and several concurrent QEMU-heavy runs manufacture their own failures.
#
# So every unheld pull request is armed on every pass, and only one is updated. Arming only the head
# of the queue left #134 sitting CLEAN with all twelve checks green behind a lower-numbered pull
# request that was still building, which is precisely the failure this script exists to end.
#
# Under the up-to-date rule the *updating* is inherently serial anyway: merging any pull request
# stales every other one, so the queue can only ever move one at a time.
#
# Name: unrecorded. Provisional, minted 2026-08-04 and not yet put to calef. Named for what it does
# to the queue rather than for the mechanism, in the family of `qemu-bounded.sh`. It lives in
# `scripts/` rather than `script/` because it is a maintainer's tool and not a front door a
# contributor types; `script/` is the normalised "Scripts to Rule Them All" set (notes/scripts.md).
# See notes/merge-queue.md.

set -e
cd "$(dirname "$0")/.."

REPO="calef/nife"
HELD_LABEL="needs-architect"
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
		echo "merge-drain: queue empty; nothing open that does not need calef"
		return 1
	fi

	# **At most one merge in flight.** This is the third shape this loop has had, and the reasoning is
	# worth keeping because each earlier one failed in a way that looked like the opposite bug.
	#
	# Arming only the head left #134 sitting CLEAN with twelve green checks behind a lower-numbered
	# pull request that was still building. So the next version armed everything, and that starved
	# the head instead: under the up-to-date rule a merge stales every other branch, so a small
	# doc-only pull request goes green during a big one's thirty-minute cycle, merges, and sends the
	# big one back to the start. #117 was re-updated twice that way before anyone noticed.
	#
	# Both failures are one fact seen from two sides: **a merge is exclusive**, so the queue can only
	# land one thing at a time and the only real question is which. Pick exactly one target, arm
	# exactly that one, leave the rest alone until it lands.
	#
	# Order: anything already CLEAN wins, because it needs nothing but its checks and lands in
	# minutes rather than in a cycle. Otherwise the lowest-numbered, which is the oldest, and which
	# is what keeps a big pull request from starving behind a stream of small ones.
	# **Whatever is already in flight finishes first, even ahead of one that is ready.** This is the
	# fourth shape and it corrects the third, which preferred a CLEAN pull request on the reasoning
	# that it lands in minutes. That reasoning is wrong under the up-to-date rule: merging the cheap
	# one *stales the one in flight*, so a five-minute merge costs a thirty-minute one a whole
	# further cycle and saves nothing, because the cheap one would have landed straight afterwards
	# anyway. #120 paid three cycles that way while #137 and #139 went past it.
	#
	# Order the two operations by what they cost the queue, not by what they cost themselves.
	flying=$(printf '%s' "$q" | jq -r '[.[] | select(.mergeStateStatus == "BLOCKED")] | .[0].number // empty')
	if [ -n "$flying" ]; then
		failed=$(gh pr view "$flying" --repo "$REPO" --json statusCheckRollup \
			-q '[.statusCheckRollup[] | select(.conclusion == "FAILURE") | .name] | join(", ")' 2>/dev/null)
		if [ -n "$failed" ]; then
			echo "merge-drain: STALLED. #$flying is failing $failed"
			return 1
		fi
		gh pr merge "$flying" --repo "$REPO" --auto --merge --delete-branch >/dev/null 2>&1 || true
		return 0
	fi

	# Nothing in flight. A pull request that is already current needs only its checks, so it goes
	# next and nothing else is touched until it lands.
	ready=$(printf '%s' "$q" | jq -r '[.[] | select(.mergeStateStatus == "CLEAN")] | .[0].number // empty')
	if [ -n "$ready" ]; then
		gh pr merge "$ready" --repo "$REPO" --auto --merge --delete-branch >/dev/null 2>&1 || true
		echo "merge-drain: #$ready is current and armed; waiting for it to land"
		return 0
	fi

	num=$(printf '%s' "$q" | jq -r '.[0].number')
	state=$(printf '%s' "$q" | jq -r '.[0].mergeStateStatus')
	title=$(printf '%s' "$q" | jq -r '.[0].title')
	gh pr merge "$num" --repo "$REPO" --auto --merge --delete-branch >/dev/null 2>&1 || true

	case "$state" in
	BEHIND | UNKNOWN)
		echo "merge-drain: updating #$num against main ($title)"
		gh api -X PUT "repos/$REPO/pulls/$num/update-branch" >/dev/null 2>&1 || true
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
