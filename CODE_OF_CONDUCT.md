# Code of conduct

This is a small project. The rules are short, and most of them exist to keep the work honest
rather than to settle arguments.

## How to behave

* Critique the work, not the person. "This parse is wrong because the length field is at +0x0E"
  is welcome; "you clearly have no idea what you are doing" is not.
* No harassment, discrimination, personal attacks, or publishing anyone's private information.
* Assume good faith, and be specific when you disagree. A claim with a file offset and a hexdump
  can be checked; an opinion cannot.
* English or Chinese are both fine, in issues and in commits.
* A "no" from the maintainer does not need to be negotiated. It usually means "not this way", not
  "never".

## Scope

Issues, pull requests, discussions, commit messages and review comments: everywhere this project
is talked about in public.

## Rules for the work

These are not etiquette, they are how the project stays correct. Each rule belongs to a subject, and
a new rule joins the subject it belongs to rather than becoming a section of its own.

### Documentation

**1.1** Documentation is organised by subject and by section. Documents live in `docs/`, one file
per subject, and each file opens with a paragraph saying what it covers — that paragraph is what
decides whether your change belongs in it.

**1.2** **A change that alters documented behaviour updates the section that documents it, in the
same commit.** `docs/format.md` is the byte-level specification of the file formats; `README.md`
carries the feature list in "Status" and "Not implemented yet". Updating one section is the normal
case: one paragraph, one table, one field row. Nothing here asks you to re-read a document end to
end, and nothing asks you to touch a document your change has nothing to do with.

**1.3** Citations move with their section. Source comments cite the specification by section
(`docs/format.md §2.6`); if a section moves or is renumbered, those citations move in the same
commit — `grep -rn "docs/format.md §"` finds all of them.

**1.4** New documents join the same rule. A new file states its subject at the top, and from then on
this rule applies to it as it applies to the others. Adding a document is not a commitment to keep
every earlier one in step with it.

If you cannot name the section your change affects, you have not finished thinking about the change.
A change to code that no document describes needs no documentation — internal refactors, tests,
build scripts.

### Evidence

**2.1** Say how sure you are. Format knowledge in this project is graded: `[verified]` means checked
exactly against every file of that kind on hand, `[strong]` means consistent but with a plausible
alternative reading, `[open]` means unproven. New facts arrive with a grade and with the evidence
that earned it. Downgrading a `[verified]` claim needs a file that contradicts it, not a hunch.

**2.2** Do not guess. An undocumented path is either preserved verbatim or reported with a clear
error. A parser that invents a field to make a file load is worse than one that refuses, because the
silent version produces plausible garbage that someone will then build on.

### Code and content

**3.1** Original code. This is a clean-room effort: do not copy code from other Kontakt players,
from Native Instruments, or from decompiled sources, and do not paste their documentation. Reading
what a public project does to understand a format is fine; lifting its implementation is not.

**3.2** Keep other people's libraries out of the repository. Libraries are commercial products and
they are not ours to redistribute: no sample files, no presets, no artwork, no library names in
comments, tests or fixtures. Tests that need a real instrument read its path from an environment
variable and skip when it is unset. The formats themselves are fair game — they are what this
project exists to describe.

### Tests

**4.1** Leave the tests green. `cargo test` passes before a change is proposed, and a new format
rule comes with the test that would have caught it earlier — that is the difference between working
on a format and guessing at one.

**4.2** Say when you last ran the end-to-end tests. The unit tests run everywhere; the ones that
need a real library are `#[ignore]`d and easy to forget, which is exactly how they break unnoticed.

## Reporting a problem

Open an issue, or write to the maintainer at <31core@tutanota.com>. If the report is about the
maintainer's own behaviour, open the issue anyway — there is nobody else to escalate to, and a
public record is better than a private complaint that goes nowhere.

The maintainer may edit or delete comments, close issues, and block accounts that break these
rules. Enforcement will be proportionate: a correction first, a block only for someone who keeps
going.
