# Experimentation opt-out policy cases

`experimentation-opt-out-policy.jsonl` contains all 90 `policy` cases from the
[shared Lean contract](https://github.com/DataDog/ffe-system-test-data/blob/f8a4176cd8360b12b56a8b6ec6ef215e2cc21077/contracts/experimentation-opt-out/corpus/browser-proposal.jsonl).

Source SHA256: `c4a1b9c9eed7197b8d66c1d3e46fcaaa4ec70143a2583b31baa04086c8f652dd`.
Extracted policy SHA256: `0d396beb2b627606f795386340d5e9d7443a40c51e84200b60527938969f7175`.

The cases cover D002, D004 revision 2, D010 for valid boolean input, and D011.
The adapter uses the production Rust evaluator. It preserves every expected result.
Timestamp grammar, provider configuration, transport, and telemetry delivery are outside this profile.

## Timestamp cases

`experimentation-opt-out-timestamps.jsonl` contains all 50 timestamp cases from
the [pinned shared corpus](https://github.com/DataDog/ffe-system-test-data/blob/59ab2bfcb9a2480762bf51451a3316e093247bd4/contracts/experimentation-opt-out/corpus/browser-proposal.jsonl),
including the seven exact examples in
[libdatadog #2698's parser review](https://github.com/DataDog/libdatadog/pull/2698#discussion_r4380967749).
The additional cases extend that corpus without changing its existing expectations.
The shared contract remains a [draft PR](https://github.com/DataDog/ffe-system-test-data/pull/55).

Extended shared-corpus SHA256: `4c4628ae30217bd2ead2ef34144f43e0163f9916ff0c8accae0847391dff64a6`.
Extracted timestamp SHA256: `f09e4061d624b196cc51d9c3936428621ca7a8b79f7a0ad1cc993f6b57bdf31c`.

The 50 cases cover D005 revision 2, D006 revision 2, and D007 revision 1.
The production evaluator checks both cutoff and activation parsing. Valid inputs
are compared with the oracle's expected instant and its adjacent representable
nanoseconds, verifying normalization and strict cutoff selection. Invalid inputs
retain their mode-specific errors; null is invalid subject input but missing
activation. No case is excluded. Existing `startAt`/`endAt` syntax is separate.
