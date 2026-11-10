# `src/fix.rs` review

**Reviewed:** 2026-11-10
**Commit:** `793304d` (`feat: complete database review follow-ups`)
**Scope:** [src/fix.rs](../src/fix.rs) (433 lines), its caller [src/cmd_fix.rs](../src/cmd_fix.rs), tests, and the project design documents.
**Verification:** `cargo test --all-targets` passes (97 unit + 3 pty tests); release-build reproduction runs; `rustfmt --check`; `cargo clippy --all-targets`.

**Verdict:** the edit-distance core is sound, but the repair pipeline is not yet safe to trust. It can silently do nothing for a typo the user has made before, drop path components and flags, rewrite valid shell syntax, and produce destructive candidates with no warning. The top priority is correctness and safety of candidate generation, not micro-optimisation.

The five changes with the highest payoff are: fix the multi-component path contract (F-01), stop learning from the failed row itself (F-02), make Rule 0 and git parsing conservative (F-03/F-04), replace whitespace splitting with shell-aware tokenisation (F-05/F-06), and enforce the sensitive/exit/destructive policy before wiring any run action (F-07/F-08).

> **Follow-up (2026-11-10):** F-01 and F-02 were fixed after this review. `missing_components` now returns the ordered missing suffix and Rule 3 reconstructs and verifies the full path; the history query now excludes failed, sensitive, and current rows, and `suggest` filters the typed token/flag from its pools. `cargo test --all-targets` now passes 100 unit, 1 fix integration, and 3 pty tests. The rest of this document is the pre-fix analysis for the outstanding findings, so line numbers may have shifted slightly.

## How this was reviewed

I read all 433 lines and traced every public entry point through `cmd_fix::run`. I compared behaviour against the repository's design documents, especially:

- [03-features.md](03-features.md#L23-L26) — F4, the fix feature contract.
- [05-intelligence-engine.md](05-intelligence-engine.md#L23-L33) — the typo-repair algorithms and path-resolution steps.
- [06-architecture.md](06-architecture.md#L127) — `%q` quoting and the no-`eval` rule.
- [07-privacy-safety-security.md](07-privacy-safety-security.md#L19-L40) — sensitive rows, destructive shapes, shell-aware parsing, no silent `sudo`.
- [08-performance-budgets.md](08-performance-budgets.md#L21) — the human latency bar.
- [01-vision-and-principles.md](01-vision-and-principles.md#L39) — accept is not run; no silent `sudo`.
- [README.md](../README.md#L29-L30) — the user-facing description of `fix`.

There is no separate coding-style guide in the repository (no `CONTRIBUTING.md`, `AGENTS.md`, or style document), so I treated those docs plus Rust's standard `fmt`/`clippy` expectations as the guide. If you meant a different guide, point me at it and I will redo the compliance matrix against it.

Reproductions below use `target/debug/ghostline` unless stated otherwise. `$BIN` and `$D` are set as follows:

```sh
BIN=target/debug/ghostline
D=$(mktemp -d)
$BIN log --db "$D/h.db" --shell bash --cwd "$D" --exit 1 --session s true
```

The `log` seed creates a database for the history-learning cases; filesystem-only examples also work when no database exists.

## Summary

| # | Severity | Finding | Key location |
|---|---|---|---|
| F-01 | **Critical** | `missing_component` returns the deepest missing component and drops the rest of the path, so many path corrections are wrong or absent | [fix.rs:161–183](../src/fix.rs#L161-L183), [fix.rs:252–274](../src/fix.rs#L252-L274) |
| F-02 | **Critical** | The failed command is fed back as its own history candidate; repeated typos return the unchanged command forever | [fix.rs:92–103](../src/fix.rs#L92-L103), [fix.rs:208–215](../src/fix.rs#L208-L215), [cmd_fix.rs:76–85](../src/cmd_fix.rs#L76-L85) |
| F-03 | **High** | Rule 0 swaps wrappers, assignments, keywords, and `!` into the middle of a command, and suppresses a better Rule 1 suggestion | [fix.rs:194–202](../src/fix.rs#L194-L202) |
| F-04 | **High** | Git parsing ignores global options, `--`, `-b`, and path operands; branch repair corrupts valid commands | [fix.rs:224–246](../src/fix.rs#L224-L246) |
| F-05 | **High** | Rule 3 treats option tokens as path operands: `ls -la downloads` becomes `ls la downloads` | [fix.rs:248–278](../src/fix.rs#L248-L278) |
| F-06 | **High** | `split_whitespace` and raw string reconstruction are not shell-aware; quoted paths fail, and suggestions are emitted without quoting or control-character sanitisation | [fix.rs:189](../src/fix.rs#L189), [fix.rs:294](../src/fix.rs#L294), [fix.rs:335–347](../src/fix.rs#L335-L347) |
| F-07 | **High** | Sensitive rows and exit-code policy are ignored; `--last` can suggest a failed sensitive command, and `--exit` is parsed but unused | [cmd_fix.rs:43–85](../src/cmd_fix.rs#L43-L85) |
| F-08 | **High** | Destructive candidates are produced without warning, contradicting the comment that the function never suggests destructive rewrites | [fix.rs:204–219](../src/fix.rs#L204-L219), [fix.rs:299–330](../src/fix.rs#L299-L330), [redact.rs:115](../src/redact.rs#L115) |
| F-09 | **Medium** | The `mkdir` rule creates the target and then runs the original command again; `cp`/`mv` target semantics are wrong for trailing slashes and `-t` | [fix.rs:280–297](../src/fix.rs#L280-L297) |
| F-10 | **Medium** | Flag repair uses unanchored `str::replacen` and treats option-like values or post-`--` operands as flags; `--flag=value` and one-letter flags are missed | [fix.rs:299–318](../src/fix.rs#L299-L318) |
| F-11 | **Medium** | Candidate order is nondeterministic because `HashSet` and `read_dir` order leak into tie-breaks | [fix.rs:114–135](../src/fix.rs#L114-L135), [fix.rs:253–257](../src/fix.rs#L253-L257) |
| F-12 | **Medium** | `cd` repair does not require a directory and mishandles `~`/`$HOME` and leading-dash names | [fix.rs:248–278](../src/fix.rs#L248-L278), [fix.rs:335–347](../src/fix.rs#L335-L347) |
| F-13 | **Medium** | `known_command` resolves slash-containing paths against the process cwd, not the supplied `cwd`, and accepts directories as runnable | [fix.rs:106–111](../src/fix.rs#L106-L111) |
| F-14 | **Medium** | `path_bins` is scanned twice for most multi-token commands, and `best` runs a full-matrix OSA for every candidate with no length prefilter | [fix.rs:195–222](../src/fix.rs#L195-L222), [fix.rs:55–82](../src/fix.rs#L55-L82) |
| F-15 | **Medium** | The `sudo` heuristic uses raw prefix matching, misses common flag forms, does not check EUID or that the command exists, and is not gated on exit status | [fix.rs:320–328](../src/fix.rs#L320-L328) |
| F-16 | **Low** | Git branch repair only sees local branches, spawns `git` from `PATH` with no timeout, and does it for every `checkout`/`switch` | [fix.rs:145–157](../src/fix.rs#L145-L157) |
| F-17 | **Low** | The builtin list is duplicated, incomplete, and divergent from `complete.rs`; `path_bins` also duplicates `complete.rs` logic with different limits | [fix.rs:84–89](../src/fix.rs#L84-L89), [fix.rs:114–135](../src/fix.rs#L114-L135) |
| F-18 | **Low** | The CLI prints `[Enter run / e edit / d dismiss]` although none of those actions exist; missing option values silently fall back to defaults | [cmd_fix.rs:18–28](../src/cmd_fix.rs#L18-L28), [cmd_fix.rs:99](../src/cmd_fix.rs#L99) |
| F-19 | **Low** | Test coverage is happy-path only; `fix.rs` has 10 clippy warnings and is not rustfmt-clean | [fix.rs:349–433](../src/fix.rs#L349-L433) |

## What is already good

These are worth preserving while fixing the rest:

- **The OSA implementation is correct.** The recurrence at [fix.rs:7–34](../src/fix.rs#L7-L34) handles insertion, deletion, substitution, and adjacent transposition, with the right matrix boundaries. The unit test for `gti`/`git` and `chekout`/`checkout` is meaningful.
- **The threshold matches the documented shape.** `max(2, len / 3)` at [fix.rs:37–39](../src/fix.rs#L37-L39) is consistent with the “short commands get a floor of two” expectation and is directly tested.
- **The tie-break intent is sensible.** `best` prefers exact case-fold matches, then history, then transpositions, then distance ([fix.rs:55–82](../src/fix.rs#L55-L82)). That is a reasonable first-generation ranking and easy to reason about.
- **The output is bounded.** Dedupe and `take(3)` at [fix.rs:330–332](../src/fix.rs#L330-L332) match F4's “max three candidates” requirement.
- **There is no execution path.** `fix.rs` only returns strings; `cmd_fix.rs` only prints them. That is the right default and consistent with `01`'s “accept ≠ run” principle, even though the CLI text currently implies otherwise.
- **Filesystem checks are conservative about errors.** `read_dir`/`exists` failures generally produce no suggestion rather than a panic.
- **`git_branches` uses `Command` arguments, not a shell string** ([fix.rs:145–157](../src/fix.rs#L145-L157)), so branch names and `cwd` are not shell-interpolated.
- **The public API is small and documented.** `suggest`, `best`, `osa`, `within_threshold`, `known_command`, and `history_verbs` are easy to test in isolation.
- **The happy-path tests are readable** and the suite is green, which gives a useful baseline for the regression tests proposed below.

## Line-by-line map

| Lines | Unit | Verdict |
|---|---|---|
| 1–2 | Imports | Fine; no unused imports reported by clippy. |
| 4–34 | `osa` | Mathematically correct OSA; not the guide's true, weighted Damerau-Levenshtein; O(n·m) time and memory. |
| 36–39 | `within_threshold` | Formula is right for ASCII; uses byte length while distances are char-based, so non-ASCII input is more lenient than intended. |
| 41–51 | `is_transposition` | Logic is correct for one adjacent swap; allocates a full diff vector instead of early-exiting after two differences. |
| 53–82 | `best` | Ranking intent is sound; input order is the hidden tie-breaker, and OSA is computed before the cheap length check. |
| 84–89 | `SHELL_BUILTINS` | Incomplete and inconsistent with [complete.rs](../src/complete.rs); missing `builtin`, `typeset`, `[[`, `]]`, `break`, `continue`, and many reserved words. |
| 92–103 | `history_verbs` | Learns from every row, including the current failure and sensitive/failed commands; first-token only. |
| 106–111 | `known_command` | Builtin/PATH lookup is fine; slash paths use the wrong base directory, and a directory passes `exists()`. |
| 114–135 | `path_bins` | Correct executable check on Unix; unbounded scan, no cache, unsorted `HashSet`, no non-Unix file check. |
| 137–142 | `GIT_SUBCOMMANDS` | Useful shortlist, but incomplete and unaware of global options or aliases. |
| 145–157 | `git_branches` | Correct command construction and failure handling; local branches only, no timeout, no remote refs/tags, runs from `PATH`. |
| 159–183 | `missing_component` | Comment says “first missing component”; code returns the **deepest** missing component. This is the root cause of F-01. |
| 185–192 | `suggest` entry | Reasonable shape; `split_whitespace` is the root of F-05/F-06. |
| 194–202 | Rule 0 | Too broad; corrupts shell wrappers/assignments/keywords and blocks Rule 1. |
| 204–222 | Rule 1 | Algorithm is reasonable, but self-history poisoning and the unconditional full PATH scan make it unreliable and slow. |
| 224–246 | Rule 2 | Works for the simple `git chekout` case; breaks on options, `--`, and path operands. |
| 248–278 | Rule 3 | Works for a typo in the final component; fails for earlier components, flags, `~`, quoting, and non-directory `cd` targets. |
| 280–297 | Rule 4 | Correct idea for `touch`/`cp`/`mv`; `mkdir` produces a redundant, failing second command; target selection is naive. |
| 299–318 | Rule 5 | Correct idea for a simple history-learned flag; replacement is not token-scoped, self-history is included, and `=`/`--`/short flags are mishandled. |
| 320–328 | Rule 6 | Narrow and unguarded; prefix matching is too literal and too broad at the same time. |
| 330–332 | Dedupe/cap | Works as intended; the “never suggest destructive rewrites” comment is false. |
| 335–347 | `pretty_path` | Relative/home compaction is useful; no quoting, no `./` protection, lossy UTF-8, and HOME is assumed rather than derived from EUID. |
| 349–433 | Tests | Good happy-path smoke tests; no tests for the bugs above, no `cmd_fix` tests, and two tests depend on `/tmp`/real `git`. |

## Findings

### F-01 — Multi-component path repair returns the wrong component (Critical)

**Where:** [fix.rs:161–183](../src/fix.rs#L161-L183), used by [fix.rs:252–274](../src/fix.rs#L252-L274).

`missing_component` walks upward from the full path, pushing components as it goes:

```rust
while !cur.exists() {
    missing.push(name);          // leaf first
    cur = cur.parent();
}
missing.into_iter().next()       // still leaf-first: the deepest missing component
```

The comment promises the first missing component (the one nearest the existing ancestor), but `next()` returns the component nearest the leaf. The caller then joins only that component to the existing ancestor, so every component below it is discarded.

**Reproductions:**

```sh
mkdir -p "$D/Documents/sub"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd Documnets/sub'
# actual:   no suggestion for: cd Documnets/sub
# expected: cd Documents/sub

mkdir -p "$D/Downloads/sub" "$D/sun"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd downloads/sub'
# actual:   did you mean: cd sun?   (the "downloads" component is dropped)
# expected: cd Downloads/sub

touch "$D/c.txt"
$BIN fix --db "$D/h.db" --cwd "$D" 'touch a/b/c.txt'
# actual first candidate: touch c.txt
# expected:               touch a/b/c.txt after creating its parent, or mkdir -p a/b && touch a/b/c.txt
```

The last example also explains why `touch a/b/c.txt` can rank a completely wrong correction first: the deepest missing component is `c.txt`, which is compared against entries in the cwd instead of against `a`.

**Fix direction:** return the existing ancestor plus an ordered list of missing components from root to leaf, e.g. `(PathBuf, Vec<String>)`. Apply the case/fuzzy repair to `missing[0]`, rebuild the full path with the remaining suffix unchanged, and verify the result exists. Do not silently truncate the suffix. Add tests for one, two, and three missing components, and for a typo in the first component.

### F-02 — History poisons itself; repeated typos become permanent no-ops (Critical)

**Where:** [fix.rs:92–103](../src/fix.rs#L92-L103), [fix.rs:208–215](../src/fix.rs#L208-L215), [fix.rs:303–315](../src/fix.rs#L303-L315), [cmd_fix.rs:76–85](../src/cmd_fix.rs#L76-L85).

The history query selects every command whose text starts with the first token, with no success or sensitivity filter:

```sql
SELECT cmd FROM history
WHERE cmd LIKE ?1 || '%'
ORDER BY started_at DESC
LIMIT 200
```

The failed command is logged before the user runs `fix`, so it is in that result. `history_verbs` adds its first token to the preferred set and to the Rule 1 pool. `best` gives an exact case-folded match the highest priority, so the bad token wins over the real command. The same thing happens to Rule 5: the bad flag is in `flag_pool`, and `best` returns it unchanged.

**Reproductions:**

```sh
$BIN log --db "$D/h.db" --shell bash --cwd "$D" --exit 127 --session s 'gti status'
$BIN fix --last --db "$D/h.db"
# did you mean: gti status?   <-- unchanged; no correction

$BIN log --db "$D/h.db" --shell bash --cwd "$D" --exit 2 --session s 'ls --colr'
$BIN fix --last --db "$D/h.db"
# did you mean: ls --colr?   <-- unchanged
```

A prior successful `git status` or `ls --color` does not rescue the result: exact self-match has higher priority than the useful candidate. This also means the feature degrades over time — once a typo has been attempted, it can keep suggesting itself.

**Fix direction:** learn only from successful, non-sensitive rows (`exit_code = 0 AND sensitive = 0`), exclude the currently diagnosed command, and explicitly reject any candidate whose token/text equals the typed token or bad flag. For `--last`, exclude the selected row from the learning set. Add an integration test that seeds a failed row and a successful row and asserts the successful vocabulary wins.

### F-03 — Rule 0 corrupts shell wrappers, assignments, and keywords (High)

**Where:** [fix.rs:194–202](../src/fix.rs#L194-L202), [fix.rs:84–89](../src/fix.rs#L84-L89).

Rule 0 swaps the first two tokens whenever token 0 is unknown and token 1 is known. That is only valid for the specific `checkout git main` shape. It does not understand shell grammar, assignments, reserved words, or wrappers.

**Observed results:**

| Input | Actual suggestion | Expected |
|---|---|---|
| `time git status` | `git time status` | no swap; `time` is a shell keyword |
| `FOO=1 git status` | `git FOO=1 status` | no swap; `FOO=1` is an assignment prefix |
| `! git status` | `git ! status` | no swap; `!` is reserved syntax |
| `builtin rm file` | `rm builtin file` | no swap; `builtin` is a shell builtin |
| `rmm file` | `file rmm` | `rm file` (Rule 1 would find it) |

The last row is particularly damaging: Rule 0 emits a bad candidate and then Rule 1 is skipped because of `out.is_empty()` at [fix.rs:207](../src/fix.rs#L207). The presence of an argument turns a useful correction into a harmful one.

**Fix direction:** do not apply Rule 0 unless both sides are simple command words: token 0 must not be an assignment, reserved word, `!`, or known wrapper; token 1 must be a known command and token 0 must be plausible as its argument. Alternatively, remove Rule 0 until a real parser exists. Either way, Rule 1 must be allowed to run even when Rule 0 produced a candidate, or the candidates must be ranked independently.

### F-04 — Git option, `--`, and branch handling corrupts valid commands (High)

**Where:** [fix.rs:224–246](../src/fix.rs#L224-L246).

Rule 2 assumes token 1 is the subcommand and token 2 is the branch. Git's real grammar has global options before the subcommand, per-subcommand options before operands, and `--` to terminate options.

**Reproductions:**

```sh
$BIN fix --db "$D/h.db" --cwd /tmp 'git -C /tmp status'
# did you mean: git gc /tmp status?
# expected: no suggestion (the command is valid)

$BIN fix --db "$D/h.db" --cwd /tmp 'git -c core.pager=cat status'
# did you mean: git gc core.pager=cat status?

# In a repository with a local branch named b:
$BIN fix --db "$D/h.db" --cwd "$REPO" 'git checkout -b newbranch'
# did you mean: git checkout b newbranch?
# expected: leave -b/newbranch intact

$BIN fix --db "$D/h.db" --cwd "$REPO" 'git checkout -- mian'
# did you mean: git checkout b mian?
# expected: no branch rewrite after --

# In a repository with a branch main and a file main.c:
$BIN fix --db "$D/h.db" --cwd "$REPO" 'git checkout main.c'
# did you mean: git checkout main?
# expected: no suggestion, or a path-aware suggestion
```

The `-b` case is especially dangerous because the suggestion can check out a different branch and then treat `newbranch` as a path. The `--` case can turn a pathspec checkout into a branch checkout.

**Fix direction:** parse git global options (`-C`, `-c`, `--git-dir`, `--work-tree`, `--namespace`, and their value forms), locate the first non-option subcommand, stop at `--`, and only consider a branch argument when it is not an option and is not obviously a path. Handle `-b`/`-B`/`-c`/`-C`/`--orphan` explicitly, and prefer no suggestion over a guess. The guide's intended signal is git's own `did you mean` stderr ([05-intelligence-engine.md](05-intelligence-engine.md#L10)); without stderr, the branch rule should be conservative.

### F-05 — Flags are treated as path operands (High)

**Where:** [fix.rs:248–278](../src/fix.rs#L248-L278).

Rule 3 assumes `toks[1]` is a path. For `ls`, `cat`, and `touch`, `toks[1]` is often an option.

```sh
mkdir -p "$D/la" "$D/Downloads"
$BIN fix --db "$D/h.db" --cwd "$D" 'ls -la downloads'
# did you mean: ls la downloads?
```

The flag `-la` is interpreted as a missing path, matched to the sibling directory `la`, and the actual `downloads` typo is never repaired. The same pattern affects `ls --color=auto downloads`, `cat -n file`, `cd -P downloads`, and similar commands.

**Fix direction:** use a shell-aware tokenizer and an option parser that knows which options take separate values. At minimum, skip tokens beginning with `-` until `--`, then treat the next token as the path. Preserve every original token when rebuilding the command; only replace the token that was diagnosed.

### F-06 — The parser and output are not shell-aware (High)

**Where:** [fix.rs:189](../src/fix.rs#L189), [fix.rs:217–219](../src/fix.rs#L217-L219), [fix.rs:262–273](../src/fix.rs#L262-L273), [fix.rs:294](../src/fix.rs#L294), [fix.rs:315](../src/fix.rs#L315), [fix.rs:335–347](../src/fix.rs#L335-L347), [cmd_fix.rs:99](../src/cmd_fix.rs#L99).

`split_whitespace` cannot see quotes, backslash escapes, command substitutions, or empty arguments. `join(" ")` and `format!` then emit raw shell text. Paths with spaces therefore cannot be repaired, and paths that do get substituted are not quoted.

**Reproductions:**

```sh
mkdir -p "$D/My Docs"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd "My Docz"'
# actual:   no suggestion for: cd "My Docz"
# expected: cd "My Docs"

$BIN fix --db "$D/h.db" --cwd "$D" 'touch "My Dir/file"'
# actual:   mkdir -p Dir && touch "My Dir/file"
# expected: mkdir -p "My Dir" && touch "My Dir/file"
```

The second example creates the wrong directory because the tokenizer sees `"My`, `Dir/file"` and derives the path from fragments. Separately:

```sh
mkdir -p "$D/-Downloads"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd -downloads'
# did you mean: cd -Downloads?   <-- cd treats this as an option
```

`pretty_path` also returns raw control characters and metacharacters from filenames, and `cmd_fix` prints candidates with `println!` without `redact::strip_controls` or shell quoting. Today the command is only printed, but [cmd_fix.rs:99](../src/cmd_fix.rs#L99) advertises `[Enter run ...]`, and `-q` is documented as “for Enter to run wiring”. If that wiring is implemented with the current strings, it is a shell-injection path; even before that, copying and pasting a suggestion can create unintended files or arguments.

**Fix direction:** implement a small shell lexer that preserves token spans and handles single/double quotes and backslashes; parse only enough grammar to identify command names, assignments, options, and path operands. Rebuild by replacing the original span, not by joining tokens. Shell-quote every substituted path (the guide's `%q` rule at [06-architecture.md](06-architecture.md#L127)), prefix `./` for names beginning with `-`, and strip C0 controls from all output. Do not wire `Enter run` until this is done and destructive candidates have a confirmation path.

### F-07 — Sensitive rows and exit status are ignored (High)

**Where:** [cmd_fix.rs:43–85](../src/cmd_fix.rs#L43-L85).

`--last` selects the most recent nonzero-exit row without `sensitive = 0`:

```sql
SELECT cmd, cwd, exit_code FROM history
WHERE exit_code != 0
ORDER BY started_at DESC LIMIT 1
```

The history-learning query has the same omission. `--exit` is parsed into `exit_code`, but the destructuring uses `_exit` and `suggest` never receives it.

**Reproductions:**

```sh
$BIN log --db "$D/h.db" --shell bash --cwd /tmp --exit 1 --session s \
  'curl --password hunter2 https://example.test'
$BIN fix --last --db "$D/h.db"
# did you mean: curl --password [REDACTED] https://example.test? ...
```

The secret is redacted before display, but the policy is broader than redaction: [07-privacy-safety-security.md](07-privacy-safety-security.md#L19) says sensitive rows are never suggested. A redacted sensitive command is still a sensitive command.

```sh
$BIN fix --db "$D/h.db" --cwd "$D" --exit 0 'ls -la downloads'
# did you mean: ls la downloads?
```

The command supposedly succeeded, yet the typo/path heuristics still fire. The default for an explicit command is also `exit_code.unwrap_or(1)`, so “no exit information” is treated as failure.

**Fix direction:** add `sensitive = 0` to both queries; add `exit_code = 0` to the learning query; exclude the current row from `--last`. Pass the selected exit code into `suggest` and gate rules by it (`127` for unknown command, `2` or a flag-specific hint for flag errors, `1` for git heuristics). If a sensitive `--last` row is chosen, print a refusal/no-candidate line rather than a suggestion. Add caller-level tests for these cases.

### F-08 — Destructive candidates are produced without warning (High)

**Where:** [fix.rs:204–219](../src/fix.rs#L204-L219), [fix.rs:299–330](../src/fix.rs#L299-L330), [redact.rs:115](../src/redact.rs#L115).

The comment at [fix.rs:330](../src/fix.rs#L330) says “Never suggest destructive rewrites (no rm/dd/chmod rules by design)”. That is not true: those names are in `PATH`, so Rule 1 can suggest them, and Rule 5 can suggest a more destructive flag from history.

**Observed:**

```sh
$BIN fix --db "$D/h.db" --cwd /tmp 'rmm'
# did you mean: rm?

$BIN fix --db "$D/h.db" --cwd /tmp 'gti push --force'
# did you mean: git push --force?

$BIN log --db "$D/h.db" --shell bash --cwd /tmp --exit 0 --session s 'git clean -fdx'
$BIN fix --db "$D/h.db" --cwd /tmp 'git clean -fd'
# did you mean: git clean -fdx?   <-- more destructive than the input
```

`redact::is_destructive` already filters force-pushes, hard resets, recursive deletes, and similar shapes for ghost suggestions, but `fix.rs` does not call it. The current CLI only prints, so this is not auto-execution; it is still a safety and trust problem, especially because the output says `[Enter run ...]`.

**Fix direction:** decide and enforce a policy. Either suppress destructive candidates for `fix`, or label them prominently and require explicit confirmation. Never auto-run a destructive candidate. Use the existing `redact` predicate rather than duplicating the shape list, and add tests for `rmm` → `rm`, force-push, `clean -fdx`, and `chmod` typos.

### F-09 — `mkdir` suggestion reruns the original command; `cp`/`mv` target handling is incomplete (Medium)

**Where:** [fix.rs:280–297](../src/fix.rs#L280-L297).

For `mkdir`, the code sets `parent = full.clone()` and then emits:

```rust
format!("mkdir -p {display} && {cmd}")
```

so it creates the whole target and then runs the original `mkdir` again.

```sh
$BIN fix --db "$D/h.db" --cwd "$D" 'mkdir a/b'
# did you mean: mkdir -p a/b && mkdir a/b?
# the second command fails: File exists
```

`mkdir -p /etc/ghostline-review` produces the same duplicate, and also bypasses the `sudo` heuristic because the prefix is `mkdir /etc/`, not `mkdir -p /etc/`.

For `cp`/`mv`, the code checks the parent of the last argument. That is right for `cp file newdir/file`, but wrong for:

- a trailing-slash target: `cp file newdir/` needs `mkdir -p newdir`, but the parent of `newdir/` is the cwd, so no suggestion is emitted;
- `cp -t newdir file`: the target directory is `newdir`, not the last argument;
- `cp --target-directory=newdir file`: same issue;
- multiple source operands: only the last token is considered.

The `parent.to_str()` gate at [fix.rs:291](../src/fix.rs#L291) silently skips non-UTF-8 parents even though `pretty_path` would handle them lossily; `let _ = dir;` is dead code on the following line.

**Fix direction:** special-case `mkdir` as `mkdir -p <target>` (or `mkdir -p <parent> && <original>` only when that is semantically right); for `cp`/`mv`, parse `-t`/`--target-directory` and trailing-slash targets; for `touch`, check the target's parent. Preserve flags and quote the display path. Add tests for each shape.

### F-10 — Flag repair uses unanchored replacement and over-broad flag detection (Medium)

**Where:** [fix.rs:299–318](../src/fix.rs#L299-L318).

`best` is applied to the first token that starts with `-` and has more than two bytes. The replacement is `cmd.replacen(bad_flag, hit, 1)`, which replaces the first textual occurrence anywhere in the command, not the diagnosed token.

**Observed:**

```sh
$BIN log --db "$D/h.db" --shell bash --cwd /tmp --exit 0 --session s 'ls x--colr --color'
$BIN fix --db "$D/h.db" --cwd /tmp 'ls x--colr --colr'
# did you mean: ls x--color --colr?
# The substring inside x--colr was changed; the actual bad flag remains.

$BIN log --db "$D/h.db" --shell bash --cwd /tmp --exit 0 --session s 'ls -- --color'
$BIN fix --db "$D/h.db" --cwd /tmp 'ls -- --colr'
# did you mean: ls -- --color?
# It rewrote a filename after --, not an option.

$BIN log --db "$D/h.db" --shell bash --cwd /tmp --exit 0 --session s 'sort -100 file'
$BIN fix --db "$D/h.db" --cwd /tmp 'sort -10 file'
# did you mean: sort -100 file?
# It treats a numeric key as a flag.
```

It also misses common cases:

- `ls --colr=auto` is not repaired when history has `--color`, because the whole `--colr=auto` string is compared and the value is not split.
- `cat -N` is not repaired (`len > 2` excludes two-character flags).
- Flags after `--` are eligible for rewriting, which changes operand semantics.

**Fix direction:** tokenize with spans, split `--flag=value` into name and value, stop flag repair at `--`, skip numeric/operand tokens, ignore known option values, and replace only the diagnosed token's span. Exclude the bad flag itself from `flag_pool` (F-02). Add tests for prefix collisions, `=value`, short flags, negative numbers, and post-`--` operands.

### F-11 — Candidate ordering is nondeterministic (Medium)

**Where:** [fix.rs:114–135](../src/fix.rs#L114-L135), [fix.rs:211–213](../src/fix.rs#L211-L213), [fix.rs:253–257](../src/fix.rs#L253-L257).

`path_bins` returns `HashSet<String>`, and Rule 1 iterates it directly. `read_dir` order is also unspecified. `best` keeps the first candidate on a complete tie, so process-to-process hash order changes the answer.

**Reproduction:** create a fake `PATH` directory containing executable `fooa` and `foob`, then type `fooo`:

```sh
# 12 quiet runs against the same inputs:
# fooa: 4 runs
# foob: 8 runs
```

Both are one edit from `fooo` and neither is preferred by history, so the result depends on hash iteration order. The same class of instability applies to case-insensitive sibling matches and equal-distance branches.

**Fix direction:** use `BTreeSet` or sort candidate names before ranking; sort `read_dir` results; add a final deterministic tie-breaker such as `(distance, exact_case, preferred, transposition, candidate_name)`. `complete.rs` already uses a sorted `BTreeSet`; reuse that convention.

### F-12 — `cd` repair suggests non-directories and mishandles `~` and leading dashes (Medium)

**Where:** [fix.rs:248–278](../src/fix.rs#L248-L278), [fix.rs:335–347](../src/fix.rs#L335-L347).

The sibling repair does not filter by type. If a regular file named `Downloads` exists, `cd downloads` becomes `cd Downloads`, which still fails. The scenarios below use independent fixtures.

```sh
touch "$D/Downloads"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd downloads'
# did you mean: cd Downloads?   <-- not a directory
```

`~` is treated as a literal path component, not expanded:

```sh
mkdir -p "$D/Downloads"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd ~/downloads'
# did you mean: cd Downloads?
# This points at $D/Downloads, not $HOME/Downloads.
```

When the cwd is not `$HOME`, the suggestion changes the directory the user meant. A leading dash is passed through unquoted and unescaped:

```sh
mkdir -p "$D/-Downloads"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd -downloads'
# did you mean: cd -Downloads?   <-- parsed as a cd option
```

**Fix direction:** for `cd`, require the repaired path to be a directory; expand only recognized `~/` and `$HOME/` prefixes (or leave them alone); prefix `./` for paths beginning with `-`; quote/escape the result. For `cat`, require a file; for `ls`/`touch`, apply their respective type rules. Add tests for each.

### F-13 — `known_command` uses the wrong cwd and accepts directories (Medium)

**Where:** [fix.rs:106–111](../src/fix.rs#L106-L111).

```rust
if prog.contains('/') {
    return Path::new(prog).exists();
}
```

For a slash-containing program, this resolves relative to the `ghostline` process's current directory, not the `cwd` passed to `suggest`. That makes a valid local executable look unknown and can trigger Rule 0:

```sh
mkdir -p "$D"
printf '#!/bin/sh\nexit 1\n' > "$D/tool"
chmod +x "$D/tool"
$BIN fix --db "$D/h.db" --cwd "$D" './tool git'
# did you mean: git ./tool?
# expected: no suggestion (./tool is known relative to --cwd)
```

`Path::new(prog).exists()` also returns true for directories, and it does not check the executable bit. A directory named `./foo` is treated as runnable; a non-executable file is accepted for slash paths but not for PATH names.

**Fix direction:** pass `cwd` into `known_command`, resolve relative paths against it, require `is_file()` plus the Unix executable bit, and share the builtin/keyword vocabulary with `complete.rs`. Add tests where the process cwd and `--cwd` differ.

### F-14 — PATH is scanned twice and every candidate gets a full OSA (Medium)

**Where:** [fix.rs:114–135](../src/fix.rs#L114-L135), [fix.rs:195–222](../src/fix.rs#L195-L222), [fix.rs:55–82](../src/fix.rs#L55-L82).

For any multi-token command, Rule 0 calls `path_bins()` at [fix.rs:196](../src/fix.rs#L196), then Rule 1 calls it again at [fix.rs:206](../src/fix.rs#L206), even when Rule 0 already found a candidate or the first token is known. Neither call is cached within the process. `best` then lowercases every candidate and runs the full-matrix OSA before the threshold check.

Measured on this checkout with a release build, warm cache, and a seeded DB (p50 over 15–20 subprocess runs; absolute numbers are machine-specific):

| Scenario | PATH entries | p50 |
|---|---|---|
| `PATH=/nonexistent` (process startup baseline) | 0 | 0.85–0.89ms |
| `fix 'ls'` (one scan) | 4,029 | 7.4ms |
| `fix 'cd Downloads'` (two scans, no correction needed) | 4,029 | 13.0ms |
| `fix 'gti status'` (two scans + 4k candidate OSA) | 4,029 | 13.5ms |
| `fix 'gti status'` against 4,000 synthetic executables | 4,000 | 11.6ms |

That is within F4's `<50ms` budget, but above the `<10ms` “feels instant” human bar in [08-performance-budgets.md](08-performance-budgets.md#L21), and the double scan is pure waste. `path_bins` is also unbounded; [complete.rs](../src/complete.rs) caps its equivalent at 20k entries and exposes a testable PATH override, while this copy has neither a cap nor an override. `osa` allocates an `(n+1) × (m+1)` matrix, so a very long first token can allocate a large amount of memory; `is_transposition` allocates a full diff vector even though it only needs to know whether there are exactly two differences.

**Fix direction:** compute the PATH set once, lazily, and only when the first token is actually unknown; reuse it for Rule 0/Rule 1. Add a cheap `abs_diff(len_a, len_b) > threshold` prefilter before OSA; replace the full matrix with a two/three-row OSA or a banded implementation; cap candidate/token lengths; share `complete.rs`'s sorted, capped command pool. Re-measure `fix` with an empty/normal/large PATH in CI if fix latency is considered part of the budget.

### F-15 — The `sudo` heuristic is too literal and too broad (Medium)

**Where:** [fix.rs:320–328](../src/fix.rs#L320-L328).

`starts_with` on a raw string produces both false negatives and false positives:

```sh
$BIN fix --db "$D/h.db" --cwd /tmp 'mkdir -p /etc/ghostline-review'
# did you mean: mkdir -p /etc/... && mkdir -p /etc/...?
# No sudo suggestion: the allowlist only contains "mkdir /etc/", not "mkdir -p /etc/".

$BIN fix --db "$D/h.db" --cwd /tmp 'systemctlfoo restart nginx'
# did you mean: systemctl restart nginx?
#   also: sudo systemctlfoo restart nginx
```

The second candidate is invalid because `systemctlfoo` is not `systemctl`; `starts_with("systemctl")` matched it. The rule also does not check `EUID != 0`, does not check that `sudo` exists, does not handle `doas`/`pkexec`, and fires regardless of exit code (so it can suggest `sudo` for a command that failed with 127 because it does not exist).

**Fix direction:** tokenize, require a token boundary, require the command to be known, check `EUID != 0`, check that a privilege-escalation tool exists, and gate on the exit code. If that is too much for a first version, omit the rule until stderr is available; a wrong sudo suggestion is worse than no suggestion. Never auto-run it; [01-vision-and-principles.md](01-vision-and-principles.md#L39) and [07-privacy-safety-security.md](07-privacy-safety-security.md#L40) explicitly forbid silent `sudo`.

### F-16 — Git branch repair is narrow and has avoidable side effects (Low)

**Where:** [fix.rs:137–157](../src/fix.rs#L137-L157).

- It reads only local branches; tags, remote refs, and detached-HEAD names are invisible, while the guide's rule calls for branches **and refs** ([05-intelligence-engine.md](05-intelligence-engine.md#L10)).
- It invokes `git` from `PATH` with no timeout and no `GIT_OPTIONAL_LOCKS=0`. In a pathological repository or a slow filesystem, `fix` can block.
- It runs for every `git checkout`/`switch`, even when the argument is an option or a path, which is part of F-04.
- It silently returns an empty list on any error, so a broken repository or missing git is indistinguishable from “no matching branch”.

**Fix direction:** use `git for-each-ref` with the relevant prefixes, set a short timeout, run only after option/operand parsing has identified a plausible branch argument, and cache the result for the duration of the suggestion. Tests should use a temporary repository and skip cleanly when `git` is unavailable.

### F-17 — Builtins and command discovery are duplicated and divergent (Low)

**Where:** [fix.rs:84–89](../src/fix.rs#L84-L89), [fix.rs:114–135](../src/fix.rs#L114-L135), [complete.rs](../src/complete.rs).

`fix.rs` has its own small builtin list and its own PATH scan. `complete.rs` already has a fuller builtin list (including `builtin`, `typeset`, `break`, `continue`, `getopts`, `mapfile`, `readarray`, and reserved words such as `if`/`then`/`fi`), a sorted `BTreeSet`, an explicit PATH override for tests, and a 20k cap. The two lists disagree, so adding a builtin to one does not fix the other.

This causes concrete false positives: `time`, `builtin`, `[[`, and reserved words look like unknown programs, which feeds Rule 0/Rule 1.

**Fix direction:** extract one `shell`/command-name module used by both fix and complete. Define the reserved-word/builtin vocabulary once, test the shared list, and expose a testable PATH scanner that accepts an explicit PATH value.

### F-18 — The CLI advertises actions it does not implement, and option parsing is forgiving in the wrong places (Low)

**Where:** [cmd_fix.rs:18–28](../src/cmd_fix.rs#L18-L28), [cmd_fix.rs:43–67](../src/cmd_fix.rs#L43-L67), [cmd_fix.rs:99](../src/cmd_fix.rs#L99).

The human output says:

```text
did you mean: ...?  [Enter run | e edit | d dismiss]
```

but `cmd_fix.rs` implements none of `Enter`, `e`, or `d`; the README says repair “only prints candidates” ([README.md](../README.md#L29-L30)). The comment on `-q` even calls it “for `Enter to run` wiring”. A user who follows the hint will type `e` or `d` at the shell prompt and run an unrelated command. Either implement the widget/actions or change the text to a copy/paste instruction.

Parsing also silently degrades:

- `--exit nope` becomes `None` and then defaults to 1;
- a trailing `--cwd` or `--db` becomes an empty/default value;
- `--last` ignores any explicit command parts;
- `--` is handled, but a single-dash unknown option is treated as a command word;
- a database open error aborts all suggestions, including the filesystem-only rules that do not need history.

**Fix direction:** validate required option values, reject malformed `--exit`, define `--last` + command precedence, and degrade gracefully when the DB is unavailable (warn to stderr, then run rules that need no history). Do not print interactive affordances that are not implemented. If `Enter run` is planned, gate it behind the F-06/F-08 fixes.

### F-19 — Tests and hygiene are below the rest of the repository (Low)

**Where:** [fix.rs:349–433](../src/fix.rs#L349-L433).

The suite has ten tests and covers the happy path for OSA, thresholds, case fixes, simple path typos, git subcommand, `mkdir -p`, and the swapped-order example. It does not cover:

- multi-component paths, flags, `--`, or quoted/escaped paths;
- self-history, sensitive rows, or exit-code gating (`cmd_fix` has no tests);
- git global options, `-b`, `--`, or path operands;
- `mkdir` duplication, `cp -t`, trailing slashes, or `touch` ranking;
- determinism, duplicate candidates, leading-dash paths, or control characters;
- `path_bins` caps/ordering, `known_command` with a different cwd, or `git_branches` failures.

`swapped_order` and `git_subcommand_fixed` use `/tmp`; `swapped_order` also relies on real `git` being installed. They should use `std::env::temp_dir()` and a fake PATH/repo or skip cleanly. The in-file tests use `process::id()` names, which is fine for parallel tests, but tests should remove fixtures on failure too.

Hygiene:

- `rustfmt --check src/fix.rs` reports diffs at the OSA recurrence, the transposition expression, `best`'s tuple, the `read_dir` let-else, and the git list. The file is not formatted.
- `cargo clippy --all-targets` reports 10 `fix.rs` warnings: two `needless_range_loop` (lines 18, 21), seven `collapsible_if` (96, 233, 249, 252, 290, 300, 339), and one `question_mark` (173). None are correctness lints, but they add noise and hide future real warnings.
- There is no `cargo fmt --check` / `clippy -D warnings` gate in the current CI workflow; adding one would prevent this drift.

**Fix direction:** add the regression tests listed below, make `fix.rs` rustfmt-clean, resolve or explicitly allow the clippy warnings, and add the format/lint gate. Test `cmd_fix` through a small integration surface or by extracting query construction into a testable function.

## Guide compliance

Status keys: **Yes** = follows the documented requirement; **Partial** = some behaviour but a material gap; **No** = contradicts or omits the requirement; **N/A** = not applicable to this module.

| Guide | Requirement | Status | Evidence |
|---|---|---|---|
| [03 F4](03-features.md#L23-L26) | Top-15 repair rules | Partial | Seven rules (0–6); several F4 rules (extension stem, EACCES-aware sudo, dismissal learning) are absent. |
| 03 F4 | `<50ms` | Yes | Measured ~13ms p50 here, within 50ms; the separate `<10ms` human bar is covered in the 08 row. |
| 03 F4 | Max 3 candidates | Yes | Dedupe + `take(3)` at [fix.rs:330–332](../src/fix.rs#L330-L332). |
| 03 F4 | Never auto-run destructive | Yes | `fix.rs` returns strings and `cmd_fix` prints them; there is no execution path. The advertised `Enter run` and missing destructive filter are F-08/F-18. |
| 03 F4 | Learn dismissals (2× / 24h) | No | No dismissal state or suppression logic anywhere in `fix.rs`/`cmd_fix.rs`. |
| [05 §2](05-intelligence-engine.md#L23) | True Damerau-Levenshtein, weighted (QWERTY/case) | No | `osa` uses unit-cost OSA, not true DL, and has no weights. |
| 05 §2 | SymSpell/BK-tree for bounded vocabularies | No | Linear scan + full OSA per candidate. |
| 05 §3 | First missing component, one scandir, reconstruct, verify | No | `missing_component` returns the deepest component and never reconstructs the suffix (F-01); one `read_dir` is used, but the result is not verified. |
| 05 §3 | Exact-lower first, then fuzzy ≤2 | Partial | Case-only lookup is first, but only for the final component; the multi-component case is broken. |
| [05 §4](05-intelligence-engine.md#L33) | Rank by similarity, frequency, recency, cwd affinity, success | No | `best` uses only exact case, history membership, transposition, and distance; no frequency/recency/dir/success. |
| 05 §5 | Persist accepted/rejected corrections | No | None. |
| [07](07-privacy-safety-security.md#L19) | Never suggest `sensitive=1` rows | No | `--last` and the history query omit `sensitive = 0` (F-07). |
| 07 | Suppress destructive shapes from suggestions | No | `redact::is_destructive` exists but is not called by `fix`; `rmm` → `rm`, force-push and `clean -fdx` candidates observed (F-08). |
| 07 | Shell-aware lexer; token-boundary checks; parse error = cautious | No | `split_whitespace`; no quote/escape/`--` handling; no parse-error concept (F-05/F-06). |
| 07 | Never auto-`sudo`, never auto-correct destructive semantics | Partial | No auto-execution exists, but the hint says `Enter run`, and the `sudo` rule is not gated or checked (F-15). |
| [06](06-architecture.md#L127) | `%q` quoting, no `eval` | No | Raw `join(" ")`/`format!` output; no shell quoting (F-06). |
| [08](08-performance-budgets.md#L21) | `<10ms` human instant bar | Partial | ~13ms p50 for `cd Downloads` with a 4k-entry PATH; within F4's 50ms but over the human bar. |
| [01](01-vision-and-principles.md#L39) | Accept ≠ run; no silent sudo | Partial | Only printing today; `Enter run` is advertised but not implemented, and the sudo heuristic exists. |
| [04](04-ux-spec.md) | One-line `[Enter run / e edit / d dismiss]` flow | Partial | Text matches, actions do not exist (F-18). |
| Rust hygiene | `cargo fmt`, `clippy` clean | No | `rustfmt --check` diffs and 10 `fix.rs` clippy warnings (F-19). |

## Performance notes

The measurements are in F-14. Two structural points are worth repeating:

1. **The path scan is paid twice for almost every command.** Rule 0 and Rule 1 each call `path_bins()`; Rule 1 calls it even when Rule 0 already produced a candidate, because the call happens before `out.is_empty()`. `fix 'cd Downloads'` does two full scans even though neither rule needs to correct `cd`.
2. **The fuzzy search is brute force.** `best` lowercases every candidate, runs a full OSA matrix, and only then applies `within_threshold`. On a 4k-entry PATH the double scan dominates, but on a large history/branch pool or long tokens the OSA cost and allocation become the risk. A length prefilter (`abs_diff > threshold`) and a short-token cap are cheap wins.

The F4 budget is 50ms, and the measured 13ms is within it. The performance issue is therefore not a gate failure; it is avoidable latency and unbounded work in a function that is meant to feel immediate. It becomes more serious if `fix` is wired into a synchronous prompt hook, where every command failure pays the scan.

## Test plan

These are the regression tests I would add before changing behaviour:

| Test | Setup | Expected |
|---|---|---|
| `multi_component_case` | `Documents/sub` exists | `cd Documnets/sub` → `cd Documents/sub` |
| `multi_component_typo_keeps_suffix` | `Downloads/sub` exists and `sun` exists | `cd downloads/sub` → `cd Downloads/sub`, never `cd sun` |
| `deep_touch_ranks_parent` | `c.txt` exists | `touch a/b/c.txt` → a suggestion containing `a/b/c.txt`/`mkdir -p a/b`, not bare `touch c.txt` |
| `flag_is_not_path` | `la/`, `Downloads/` exist | `ls -la downloads` → `ls -la Downloads`, not `ls la downloads` |
| `quoted_path_typo` | `My Docs/` exists | `cd "My Docz"` → `cd "My Docs"` |
| `escaped_path_typo` | `My Docs/` exists | `cd My\ Docz` → a quoted/escaped `My Docs` suggestion |
| `assignment_not_swapped` | none | `FOO=1 git status` → no `git FOO=1 status` |
| `time_keyword_not_swapped` | none | `time git status` → no `git time status` |
| `rule_zero_does_not_block_rule_one` | none | `rmm file` → `rm file`, not `file rmm` |
| `git_global_option` | any cwd | `git -C /tmp status` → no `git gc ...` candidate |
| `git_branch_flag` | repo with branch `b` | `git checkout -b newbranch` → no suggestion that drops `-b` |
| `git_double_dash` | repo with branch `b` | `git checkout -- mian` → no branch rewrite after `--` |
| `git_path_operand` | branch `main`, file `main.c` | `git checkout main.c` → no branch rewrite |
| `self_history` | log failed `gti status` and successful `git status` | suggestion is `git status`, not `gti status` |
| `sensitive_last` | log failed sensitive command | no candidate/refusal, not a redacted suggestion |
| `exit_zero_gate` | explicit `--exit 0` | no typo/path candidate |
| `mkdir_not_duplicated` | empty dir | `mkdir a/b` → `mkdir -p a/b`, not a second `mkdir a/b` |
| `mkdir_flagged_sudo` | `/etc/...` missing | `mkdir -p /etc/x` → `sudo mkdir -p /etc/x` or no candidate, not a duplicate |
| `cp_target_directory` | missing `newdir/` | `cp file newdir/` → `mkdir -p newdir && cp ...` |
| `flag_token_scope` | history `ls x--colr --color` | `ls x--colr --colr` → correct the actual flag, not the `x--colr` substring |
| `flag_equals` | history `ls --color=auto` | `ls --colr=auto` → `ls --color=auto` or no candidate |
| `flag_after_double_dash` | history `ls -- --color` | `ls -- --colr` → no rewrite |
| `cd_file_rejected` | file `Downloads` | `cd downloads` → no candidate |
| `cd_tilde_preserved` | `$HOME/Downloads` and cwd elsewhere | `cd ~/downloads` → `cd ~/Downloads` or no candidate, not `cd Downloads` |
| `leading_dash` | dir `-Downloads` | `cd -downloads` → `cd ./-Downloads`/`cd -- -Downloads`, not `cd -Downloads` |
| `known_command_cwd` | `D/tool` executable, process cwd elsewhere | `fix --cwd D './tool git'` → no `git ./tool` swap |
| `deterministic_ties` | fake PATH with `fooa`/`foob` | repeated calls return the same candidate |
| `destructive_policy` | none | destructive candidate is suppressed or explicitly labelled, never silently presented as safe |
| `output_sanitised` | candidate/path contains ESC/control | output contains no raw C0 control bytes |
| `db_unavailable` | unreadable/missing DB | filesystem rules still run, with a stderr warning |

## Suggested fix order

1. **Make the feature truthful and safe:** F-01 (path reconstruction), F-02 (self-history), F-03 (Rule 0), F-04 (git parsing), F-05/F-06 (tokenisation/quoting), F-07 (sensitive/exit), F-08 (destructive policy). Do not wire `Enter run` before this group is done.
2. **Make the common corrections correct:** F-09 (`mkdir`/`cp`/`mv`), F-10 (flag token scope), F-12 (type/`~`/dash), F-13 (`known_command`).
3. **Make it deterministic and cheap:** F-11 (ordering), F-14 (single scan, length prefilter, capped OSA), F-15/F-16 (sudo/git conservatism), F-17 (shared command vocabulary).
4. **Make it maintainable:** F-18 (CLI truthfulness/degradation), F-19 (tests, fmt, clippy, CI gate).

## Reproduction appendix

The shortest setup used for the outputs above:

```sh
BIN=/mnt/data/DOWSE/target/debug/ghostline
D=$(mktemp -d)
$BIN log --db "$D/h.db" --shell bash --cwd "$D" --exit 1 --session s true
```

Path cases:

```sh
mkdir -p "$D/Documents/sub" "$D/Downloads/sub" "$D/sun" "$D/la"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd Documnets/sub'
$BIN fix --db "$D/h.db" --cwd "$D" 'cd downloads/sub'
$BIN fix --db "$D/h.db" --cwd "$D" 'ls -la downloads'
```

Self-history and flag cases:

```sh
$BIN log --db "$D/h.db" --shell bash --cwd "$D" --exit 127 --session s 'gti status'
$BIN log --db "$D/h.db" --shell bash --cwd "$D" --exit 0 --session s 'git status'
$BIN fix --last --db "$D/h.db"

$BIN log --db "$D/h.db" --shell bash --cwd "$D" --exit 0 --session s 'ls x--colr --color'
$BIN fix --db "$D/h.db" --cwd "$D" 'ls x--colr --colr'
```

Git cases:

```sh
REPO="$D/repo"
git init -q "$REPO"
git -C "$REPO" -c user.email=a@b.c -c user.name=a commit --allow-empty -qm x
git -C "$REPO" branch b
git -C "$REPO" branch main
touch "$REPO/main.c"

$BIN fix --db "$D/h.db" --cwd "$REPO" 'git checkout -b newbranch'
$BIN fix --db "$D/h.db" --cwd "$REPO" 'git checkout -- mian'
$BIN fix --db "$D/h.db" --cwd "$REPO" 'git checkout main.c'
$BIN fix --db "$D/h.db" --cwd "$REPO" 'git -C /tmp status'
```

Sensitive and exit status:

```sh
$BIN log --db "$D/h.db" --shell bash --cwd /tmp --exit 1 --session s \
  'curl --password hunter2 https://example.test'
$BIN fix --last --db "$D/h.db"
$BIN fix --db "$D/h.db" --cwd "$D" --exit 0 'ls -la downloads'
```

Shell parsing:

```sh
mkdir -p "$D/My Docs"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd "My Docz"'
$BIN fix --db "$D/h.db" --cwd "$D" 'touch "My Dir/file"'
mkdir -p "$D/-Downloads"
$BIN fix --db "$D/h.db" --cwd "$D" 'cd -downloads'
```

All reproductions were run against the pre-fix debug binary; the timing table used the release binary. F-01 and F-02 are now fixed in the working tree; the remaining findings were not addressed by that patch.
