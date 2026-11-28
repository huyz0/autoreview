#!/usr/bin/env python3
"""Replay every pinned manifest from isolated upstream worktrees."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import subprocess
import sys

import yaml

from redicloud import prepare_redicloud

ROOT = Path(__file__).resolve().parents[2]
BENCHMARKS = ROOT / "benchmarks"
SOURCE = re.compile(r"https://github\.com/([\w.-]+/[\w.-]+)/(?:commit|blob)/[0-9a-f]{40}(?:/[^\s]*)?")
COMMIT = re.compile(r"[0-9a-f]{40}")
PINNED_MANIFESTS = {
    "azure-iot-matches", "checkout-promise", "cilium-swallowed-error",
    "dart-code-promise-executor", "go-tools", "home-assistant-blocking-bridge",
    "iceberg-go", "jplag-trim", "kubestellar-promise-executor",
    "llm-bridge-tls-verification", "mattermost-cross-file-error", "neko-cookie-catch",
    "niverwizard-msw-delay", "nokia-helm-cancellation", "redicloud-coroutines",
    "sentry-async-executor", "sftp-promise", "silphscope-digest",
    "synthea-string-equality", "syzkaller-swallowed-error", "tokio-asserted-matches",
    "uwas-rename", "vaultwarden-cli-tls-verification", "wesley-promise-executor",
    "yq-error", "yq-multi-error", "zod-aot-keyed-cleanup",
}
TRUSTED_BUILDS = {
    "checkout-promise": ("actions/checkout", "c952173edf28a2bd22e1a4926590c1ac39630461", "5a4ac9002d0be2fb38bd78e4b4dbde5606d7042f"),
    "redicloud-coroutines": ("RediCloud/cloud-v2", "ddc83b5d138cd4ba4927bd6e6b858e79bc42bb8d", "77c38cb88f1632978cb7fae22cd2093ad166908a"),
    "sftp-promise": ("jmwerk/SFTPresso", "be43f28401e0ba693e9bfe0a1b27476d9eb4117a", "460fa47433bf6ed423cc84cb0939ce6b90246e6a"),
}


def run(args, *, cwd, env=None):
    return subprocess.run(
        args, cwd=cwd, env=env, check=True, timeout=900,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8",
    ).stdout


def manifest_inputs(manifest):
    cases = yaml.safe_load(manifest.read_text(encoding="utf-8"))["cases"]
    if not cases:
        raise ValueError(f"{manifest.name} has no cases")
    repositories, pairs = set(), set()
    for case in cases:
        source = SOURCE.fullmatch(case["evidence"]["source"])
        if not source or case["evidence"]["kind"] != "historical":
            raise ValueError(f"{manifest.name} requires pinned GitHub historical evidence")
        repositories.add(source[1])
        pair = case["before"]["commit"], case["after"]["commit"]
        if not all(COMMIT.fullmatch(commit) for commit in pair):
            raise ValueError(f"{manifest.name} requires full commit IDs")
        pairs.add(pair)
    if len(repositories) != 1:
        raise ValueError(f"{manifest.name} must refer to one upstream repository")
    repository = repositories.pop()
    trusted = TRUSTED_BUILDS.get(manifest.stem)
    if trusted and (repository != trusted[0] or pairs != {trusted[1:]}):
        raise ValueError(f"{manifest.name} changed its trusted build inputs")
    return repository, pairs, len(cases)


def checkout(repo, upstream, pairs):
    repo.mkdir()
    run(["git", "init", "--quiet", "--template="], cwd=repo)
    after = sorted({fixed for _, fixed in pairs})
    run(["git", "fetch", "--quiet", "--no-tags", "--depth=2",
         f"https://github.com/{upstream}.git", *after], cwd=repo)
    for before, fixed in sorted(pairs):
        run(["git", "merge-base", "--is-ancestor", before, fixed], cwd=repo)
    run(["git", "checkout", "--quiet", "--detach", after[0]], cwd=repo)


def node_environment(name, repo, env):
    run(["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund"], cwd=repo)
    if name == "sftp-promise":
        env["AUTOREVIEW_TYPESCRIPT_MODULE"] = str(repo / "node_modules/typescript")
        return
    tool = repo / ".replay-typescript"
    run(["npm", "install", "--prefix", tool, "--no-save", "--ignore-scripts",
         "--no-audit", "--no-fund", "typescript@5.6.3"], cwd=repo)
    env.update(
        AUTOREVIEW_TYPESCRIPT_MODULE=str(BENCHMARKS / "typescript-legacy-catch.cjs"),
        AUTOREVIEW_BENCHMARK_TYPESCRIPT_MODULE=str(tool / "node_modules/typescript"),
        AUTOREVIEW_TYPESCRIPT_TRUST_ACTIONS_CORE_1_1_3="1",
        AUTOREVIEW_TYPESCRIPT_TRUST_NODE_12_22_12=os.environ["REPLAY_NODE12"],
        AUTOREVIEW_TYPESCRIPT_TRUST_EMITTED_GRAPH="1",
    )


def configure_pack(repo, name):
    (repo / ".autoreview").mkdir(exist_ok=True)
    pack = BENCHMARKS / "packs" / name
    config = {"packs": [{"id": f"{name}-benchmark", "source": {"kind": "local", "path": str(pack)}}]}
    (repo / ".autoreview/rulepacks.yaml").write_text(json.dumps(config), encoding="utf-8")


def go_environment(repo, pairs, env):
    env.update(GOMODCACHE=str(repo / ".replay-go"), GOTOOLCHAIN="local", GOWORK="off")
    revisions = sorted({commit for pair in pairs for commit in pair})
    for revision in revisions:
        snapshot = repo / ".replay-tmp" / revision
        run(["git", "worktree", "add", "--quiet", "--detach", snapshot, revision], cwd=repo)
        run(["go", "mod", "download", "all"], cwd=snapshot, env=env)
    env.update(
        GOPROXY="off", GOSUMDB="off",
        AUTOREVIEW_GO_ERROR_PROBE=os.environ["REPLAY_GO_PROBE"],
        AUTOREVIEW_GO_BINARY=os.environ["REPLAY_GO_BINARY"],
    )


def prepare_environment(manifest, repo, pairs):
    # Snapshots need the parent checkout's node_modules for TypeScript resolution.
    temp = repo / ".replay-tmp"
    temp.mkdir()
    env = {key: value for key, value in os.environ.items() if not key.startswith("AUTOREVIEW_")}
    env.update(TMPDIR=str(temp), TMP=str(temp), TEMP=str(temp))
    name = manifest.stem
    if name in {"checkout-promise", "sftp-promise"}:
        node_environment(name, repo, env)
    elif name in {"mattermost-cross-file-error", "yq-error", "yq-multi-error"}:
        go_environment(repo, pairs, env)
    elif name == "redicloud-coroutines":
        env["AUTOREVIEW_KOTLIN_CALLBACK_PROBE_CONFIG"] = str(prepare_redicloud(ROOT, repo))
    if name in {"checkout-promise", "zod-aot-keyed-cleanup"}:
        configure_pack(repo, name)
    return env


def check_report(report, count):
    coverage = report["coverage"]
    if (report["failures"] or len(report["cases"]) != count
            or sum(row["attempted"] for row in coverage) != count
            or any(row["completed"] != row["attempted"] or row["incomplete"] or row["failed"] for row in coverage)):
        raise ValueError("replay did not complete every manifest case")
    if any(row["falsePositives"] or row["falseNegatives"] for row in report["rules"]):
        raise ValueError("replay did not match the expected findings")


def replay(manifest, binary, output):
    name = manifest.stem
    repo = output / "worktrees" / name
    try:
        upstream, pairs, count = manifest_inputs(manifest)
        checkout(repo, upstream, pairs)
        env = prepare_environment(manifest, repo, pairs)
        result = subprocess.run(
            [binary, "rules", "replay", "--suite", manifest], cwd=repo,
            env=env, timeout=900, capture_output=True, text=True, encoding="utf-8",
        )
        (output / f"{name}.json").write_text(result.stdout, encoding="utf-8")
        (output / f"{name}.stderr").write_text(result.stderr, encoding="utf-8")
        result.check_returncode()
        check_report(json.loads(result.stdout), count)
        print(f"{name}: {count}/{count} completed", flush=True)
        return True
    except (OSError, ValueError, KeyError, TypeError, yaml.YAMLError, subprocess.SubprocessError) as error:
        log = getattr(error, "output", None) or getattr(error, "stderr", None)
        (output / f"{name}.failure").write_text(f"{error}\n{log or ''}", encoding="utf-8")
        print(f"{name}: {error}\n{log or ''}", file=sys.stderr, flush=True)
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    (output / "worktrees").mkdir(parents=True)
    manifests = sorted(BENCHMARKS.glob("*.yaml"))
    found = {manifest.stem for manifest in manifests}
    if found != PINNED_MANIFESTS:
        missing = sorted(PINNED_MANIFESTS - found)
        extra = sorted(found - PINNED_MANIFESTS)
        raise ValueError(f"pinned manifest inventory changed (missing={missing}, extra={extra})")
    with ThreadPoolExecutor(max_workers=3) as pool:
        successes = list(pool.map(lambda manifest: replay(manifest, binary, output), manifests))
    print(f"{sum(successes)}/{len(manifests)} manifests passed")
    return 0 if all(successes) else 1


if __name__ == "__main__":
    sys.exit(main())
