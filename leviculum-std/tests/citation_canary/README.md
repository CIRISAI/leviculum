Fixture bodies for the standing canary in `leviculum-std/tests/doc_citations.rs`.

They are the guard's own input, not part of the corpus it guards, and are
excluded from the source scan by path component. `canary_citations.rs.in`
holds four citations into `canary_target.rs.in`: one correct, one drifted
past the identifier window, one to a file that does not exist, and one into
a `reference/` submodule that is absent from the fixture tree. The guard
must report exactly the last three, and must classify the fourth as absent
rather than drifted.

Editing either file changes what the canary proves. If a change makes the
canary fail, the guard is what needs looking at, not the fixture.
