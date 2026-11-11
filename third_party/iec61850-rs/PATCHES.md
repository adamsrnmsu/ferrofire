# ferrofire patches to iec61850-rs 0.4.0

Upstream: https://github.com/hupe1980/iec61850-rs, `MIT OR Apache-2.0` (LICENSE-MIT, LICENSE-APACHE unchanged). This is the crates.io 0.4.0
source tree plus the changes below; the workspace uses it through `[patch.crates-io]`. Drop the directory and the patch section once
upstream releases the fix (bead ww-otj.68).

1. `src/proto/mms/association.rs`, `answer_associate`: the Initiate response negotiated `dataStructureNestingLevel` 5 whatever the client
   proposed. ISO 9506 negotiates down, so it is `min(proposed, 5)` now (and absent when not proposed), and `version` is `min(proposed, 1)`.
   Test: `the_negotiated_nesting_level_does_not_exceed_the_proposed_one`. (`localDetail` and the outstanding-call limits were already
   negotiated down by 0.4.0.)

Not patched here, corrected in ferrofire (`src/fd/mms.rs`, `encode`): `Answer::NOT_FOUND` carries DataAccessError 10 in the ServiceError
`access` class, where object-non-existent is 2.
