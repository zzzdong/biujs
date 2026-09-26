#!/usr/bin/env python3
"""Classify the failures of a full test262 run into "KPI noise" buckets.

Phase 2 §3 records two families that are *outside the ES6 target* but still run
and still count as failures:

* **RegExp-driven** tests — the engine has no regular-expression
  implementation (first phase ruled it out), yet every `String.prototype`
  `replace` / `match` / `search` / `split` test that passes a regex still
  executes and fails.
* **Post-ES6 APIs** — the pinned test262 already ships ES2017+ methods
  (`Object.fromEntries`, `Array.prototype.toSorted`, `Object.groupBy`, …). The
  engine implements a few of them opportunistically (`hasOwn`, `at`,
  `getOrInsert`, the `set-methods` operators), so the bucket is decided by what
  the *test source calls*, not by the proposal the test belongs to.

Target A1 ("M7 范围内失败 ≤ 800") is not measurable while those sit in the same
bucket as real gaps, so this script measures them apart. It never touches the
runner's skip table — it only reports.

Usage:

    ulimit -v 6000000
    TEST262_FAILURES=99999 cargo test --release --test test262_runner -- --nocapture > /tmp/full.txt
    ./scripts/kpi-noise.py /tmp/full.txt

The classification is a **heuristic over the test source**, so the limits are
stated up front:

* a file is *regexp* when it mentions a RegExp literal or one of the regex-ish
  APIs (`RegExp`, `.exec(`, `.test(`, `Symbol.match`, …) anywhere in its
  source. A file that needs a regex *and* trips over a real ES6 gap is listed
  here — the bucket answers "could this test pass without a regex engine?";
* a file is *post-ES6* when it calls one of the APIs in `MISSING_APIS`, i.e.
  something the engine does not implement at all. Usage elsewhere in the file
  counts, so a test that merely *checks* such a call is bucketed too;
* **`class-fields-public` / `class-static-fields-public` (489 failures) are
  deliberately left in `real`**: §3 forbids skipping them because their
  failures mix ES2022 syntax with in-scope features (destructuring, computed
  keys, generators). The script prints their count separately so the report can
  mention them without hiding them;
* `unreadable` means the path could not be opened — treat it as a bug in this
  script, not in the engine.
"""

import collections
import os
import re
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TEST_ROOT = os.path.join(REPO_ROOT, "tests", "test262", "test")

# ── RegExp: anything that needs a regex engine to do its job ──────────────
REGEXP_MARKERS = [
    "new RegExp",
    "RegExp(",
    ".exec(",
    ".test(",
    "Symbol.match",
    "Symbol.replace",
    "Symbol.search",
    "Symbol.split",
    "regexp-",
    "RegExp.prototype",
]
# A RegExp literal, loosely: `= /…/`, `(/…/`, `,/…/`, `: /…/`, `return /…/`.
REGEX_LITERAL = re.compile(r"(?:[=(,:]|return|typeof|!)\s*/[^/*\n][^\n]*/[gimsuy]*")

# ── Post-ES6 APIs the engine genuinely lacks ──────────────────────────────
# Only calls that cannot work today belong here: the ones already implemented
# (`Object.hasOwn`, `Array.prototype.at`, `Map.groupBy`, the `set-methods`
# operators, `getOrInsert`) are deliberately absent.
MISSING_APIS = [
    "Object.fromEntries",
    "Object.getOwnPropertyDescriptors",
    "Object.groupBy",
    "Map.groupBy",
    ".flat(",
    ".flatMap(",
    ".toSorted(",
    ".toReversed(",
    ".toSpliced(",
    ".with(",
    ".findLast(",
    ".findLastIndex(",
    "Math.sumPrecise",
    "Promise.withResolvers",
    "structuredClone",
    "Symbol.dispose",
    "Symbol.asyncDispose",
    "Temporal.",
    "BigInt(",
    "ArrayBuffer.prototype.transfer",
    "resizable",
]

# Feature tags that mean "this test is about ES2022+ class fields". Reported
# separately, never removed from the in-scope count (see §3).
CLASS_FIELDS_TAGS = ["class-fields-public", "class-static-fields-public"]


def resolve(test_id: str) -> str:
    """The runner prints ids as `tests/test262/test/<path>`; accept both forms."""
    if test_id.startswith("tests/"):
        return os.path.join(REPO_ROOT, test_id)
    return os.path.join(TEST_ROOT, test_id)


def strip_comments(source: str) -> str:
    """Crude comment removal: a regex literal inside a comment is not usage."""
    source = re.sub(r"/\*.*?\*/", "", source, flags=re.S)
    return re.sub(r"(?m)^\s*//.*$", "", source)


def features_of(source: str) -> list:
    match = re.search(r"^features:\s*\[(.*?)\]", source, re.M | re.S)
    if not match:
        return []
    return [part.strip() for part in match.group(1).split(",") if part.strip()]


def classify(path: str):
    """Return `(bucket, matched_apis)`."""
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            source = handle.read()
    except OSError:
        return "unreadable", []

    features = features_of(source)
    body = strip_comments(source)

    if any(f.lower().startswith("regexp-") for f in features) or any(
        marker in body for marker in REGEXP_MARKERS
    ):
        return "regexp", []
    if REGEX_LITERAL.search(body):
        return "regexp", []

    hits = [api for api in MISSING_APIS if api in body]
    if hits:
        return "post-es6", hits
    return "real", []


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2

    failing = []
    with open(sys.argv[1], "r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if line.startswith("  FAIL "):
                failing.append(line[len("  FAIL ") :].strip().split(": ", 1)[0])

    buckets = collections.Counter()
    apis = collections.Counter()
    by_dir = collections.defaultdict(collections.Counter)
    class_fields = 0
    for test_id in failing:
        path = resolve(test_id)
        bucket, hits = classify(path)
        buckets[bucket] += 1
        for hit in hits:
            apis[hit] += 1
        directory = os.path.dirname(test_id).replace("tests/test262/test/", "")
        by_dir[bucket][directory] += 1
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                features = features_of(handle.read())
                if any(tag in feature for feature in features for tag in CLASS_FIELDS_TAGS):
                    class_fields += 1
        except OSError:
            pass

    total = sum(buckets.values())
    print(f"failures classified: {total}")
    for bucket in ("real", "regexp", "post-es6", "unreadable"):
        count = buckets[bucket]
        pct = (100.0 * count / total) if total else 0.0
        print(f"  {bucket:<11} {count:>5}  ({pct:.1f}%)")
    print(f"\nof the 'real' bucket, {class_fields} carry a public-class-fields tag")
    print("(ES2022 syntax mixed with in-scope features — kept in 'real' on purpose, §3)")

    if apis:
        print("\npost-ES6 APIs hit:")
        for api, count in apis.most_common(15):
            print(f"  {count:>5}  {api}")

    for bucket in ("regexp", "post-es6"):
        print(f"\ntop directories in '{bucket}':")
        for directory, count in by_dir[bucket].most_common(12):
            print(f"  {count:>5}  {directory}")

    print("\ntop directories in 'real' (the ES6 gaps worth working on):")
    for directory, count in by_dir["real"].most_common(15):
        print(f"  {count:>5}  {directory}")

    tags = collections.Counter()
    for test_id in failing:
        try:
            with open(resolve(test_id), "r", encoding="utf-8", errors="replace") as handle:
                for feature in features_of(handle.read()):
                    tags[feature] += 1
        except OSError:
            continue
    print("\ntop feature tags among the failures:")
    for feature, count in tags.most_common(30):
        print(f"  {count:>5}  {feature}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
