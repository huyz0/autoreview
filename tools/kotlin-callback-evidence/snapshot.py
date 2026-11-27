#!/usr/bin/env python3
"""Run the experimental Kotlin probe on explicit copied inputs."""

import argparse
import hashlib
import json
import os
import pathlib
import re
import subprocess
import tempfile
import zipfile

SOURCE_LIMIT = 4 * 1024 * 1024
TOTAL_SOURCE_LIMIT = 16 * 1024 * 1024
JAR_LIMIT = 64 * 1024 * 1024
TOTAL_JAR_LIMIT = 256 * 1024 * 1024
OUTPUT_LIMIT = 1024 * 1024
CLASS_LIMIT = 4 * 1024 * 1024
JAR_ENTRIES_LIMIT = 200_000


def resolve_unique_classes(jars: list[dict], classes: list[str]) -> list[dict]:
    if len(classes) > 128 or len(set(classes)) != len(classes):
        raise ValueError("invalid required class list")
    pattern = re.compile(r"(?:[A-Za-z_$][A-Za-z0-9_$]*/)*[A-Za-z_$][A-Za-z0-9_$]*\.class\Z")
    if any(not pattern.fullmatch(name) for name in classes):
        raise ValueError("invalid required class name")
    required = set(classes)
    found = {}
    for jar in jars:
        with zipfile.ZipFile(jar["snapshot"]) as archive:
            entries = archive.infolist()
            if len(entries) > JAR_ENTRIES_LIMIT:
                raise ValueError("too many JAR entries")
            for entry in entries:
                if entry.filename.startswith("META-INF/versions/"):
                    versioned = entry.filename[len("META-INF/versions/"):].partition("/")[2]
                    if versioned in required:
                        raise ValueError(f"multi-release class is ambiguous: {versioned}")
                if entry.filename not in required:
                    continue
                if entry.filename in found or entry.file_size > CLASS_LIMIT:
                    raise ValueError(f"duplicate or oversized class: {entry.filename}")
                with archive.open(entry) as stream:
                    data = stream.read(CLASS_LIMIT + 1)
                if len(data) > CLASS_LIMIT:
                    raise ValueError(f"oversized class: {entry.filename}")
                found[entry.filename] = {"name": entry.filename, "jarSha256": jar["sha256"], "classSha256": digest(data)}
    if len(found) != len(classes):
        raise ValueError("required class missing")
    return [found[name] for name in classes]


def read_limited(path: pathlib.Path, limit: int) -> bytes:
    with path.open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"input exceeds limit: {path}")
    return data


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def resolve_input(path: pathlib.Path) -> pathlib.Path:
    if os.name == "nt":
        absolute = os.path.abspath(path)
        if len(absolute) >= 240 and not absolute.startswith("\\\\?\\"):
            absolute = "\\\\?\\UNC\\" + absolute[2:] if absolute.startswith("\\\\") else "\\\\?\\" + absolute
        return pathlib.Path(absolute).resolve(strict=True)
    return path.resolve(strict=True)


def snapshot_inputs(paths: list[pathlib.Path], directory: pathlib.Path, suffix: str, limit: int, total_limit: int) -> list[dict]:
    if not paths or len(paths) > 512:
        raise ValueError("invalid input count")
    records = []
    total = 0
    seen = set()
    for index, original in enumerate(paths):
        original = resolve_input(original)
        if not original.is_file() or original in seen or original.suffix.lower() != suffix or (suffix == ".jar" and os.pathsep in original.name):
            raise ValueError(f"invalid or duplicate input: {original}")
        seen.add(original)
        data = read_limited(original, limit)
        total += len(data)
        if total > total_limit:
            raise ValueError("total input exceeds limit")
        target = directory / str(index) / original.name
        target.parent.mkdir(parents=True)
        target.write_bytes(data)
        records.append({"original": original, "snapshot": target, "sha256": digest(data)})
    return records


def argument(value: str) -> str:
    # Kotlin's argument-file parser accepts quoted arguments with escaped
    # backslashes and quotes. Reject line breaks to preserve one argument per line.
    if "\n" in value or "\r" in value:
        raise ValueError("newline in compiler argument")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def verified_report(report_path: pathlib.Path, sources: list[dict], inputs: list[dict]) -> dict:
    report = json.loads(read_limited(report_path, OUTPUT_LIMIT))
    if report.get("version") != 2 or report.get("compiler") not in ("2.2.20", "2.3.21") or not isinstance(report.get("records"), list):
        raise ValueError("invalid compiler evidence")
    for item in inputs:
        if digest(read_limited(item["original"], item["limit"])) != item["sha256"] or digest(read_limited(item["snapshot"], item["limit"])) != item["sha256"]:
            raise ValueError("input changed during compilation")
    by_path = {item["snapshot"].resolve(): item for item in sources}
    lengths = {item["snapshot"].resolve(): len(read_limited(item["snapshot"], SOURCE_LIMIT).decode("utf-8").encode("utf-16-le")) // 2 for item in sources}
    for record in report["records"]:
        record_path = pathlib.Path(record["path"]).resolve()
        item = by_path.get(record_path)
        if item is None or record.get("sha256") != item["sha256"]:
            raise ValueError("evidence source mismatch")
        length = lengths[record_path]
        positions = [record.get(name) for name in ("callStartUtf16", "lambdaStartUtf16", "lambdaEndUtf16", "callEndUtf16")]
        if any(type(value) is not int for value in positions) or not 0 <= positions[0] <= positions[1] <= positions[2] <= positions[3] <= length:
            raise ValueError("invalid evidence span")
        record["path"] = str(item["original"])
    return report


def run(kotlinc: pathlib.Path, plugin: pathlib.Path, sources: list[pathlib.Path], jars: list[pathlib.Path], output: pathlib.Path, jvm_target: str = "1.8", required_classes: list[str] | None = None) -> None:
    if jvm_target not in ("1.8", *(str(version) for version in range(9, 26))):
        raise ValueError("unsupported JVM target")
    kotlinc = resolve_input(kotlinc)
    if output.exists():
        raise FileExistsError(output)
    with tempfile.TemporaryDirectory(prefix="autoreview-kotlin-snapshot-") as temporary:
        root = pathlib.Path(temporary)
        copied_sources = snapshot_inputs(sources, root / "sources", ".kt", SOURCE_LIMIT, TOTAL_SOURCE_LIMIT)
        dependencies = snapshot_inputs(jars, root / "jars", ".jar", JAR_LIMIT, TOTAL_JAR_LIMIT)
        resolved_classes = resolve_unique_classes(dependencies, required_classes or []) if required_classes else []
        copied_plugin = snapshot_inputs([plugin], root / "plugin", ".jar", JAR_LIMIT, JAR_LIMIT)
        for item in copied_sources:
            item["limit"] = SOURCE_LIMIT
        for item in dependencies + copied_plugin:
            item["limit"] = JAR_LIMIT
        report_path = root / "evidence.json"
        args = ["-no-stdlib", "-no-reflect", "-jvm-target", jvm_target, "-cp", os.pathsep.join(str(item["snapshot"]) for item in dependencies),
                "-d", str(root / "classes"), f'-Xplugin={copied_plugin[0]["snapshot"]}'] + [str(item["snapshot"]) for item in copied_sources]
        args_path = root / "args.txt"
        args_path.write_text("\n".join(argument(item) for item in args), encoding="utf-8")
        env = os.environ.copy()
        env["AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT"] = str(report_path)
        subprocess.run([str(kotlinc), f"@{args_path}"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, timeout=120)
        report = verified_report(report_path, copied_sources, copied_sources + dependencies + copied_plugin)
        manifest = lambda items: [{"path": str(item["original"]), "sha256": item["sha256"]} for item in items]
        result = {"snapshotVersion": 1, "jvmTarget": jvm_target, "sources": manifest(copied_sources), "dependencies": manifest(dependencies),
                  "plugin": manifest(copied_plugin)[0], "evidence": report, "resolvedClasses": resolved_classes}
        encoded = json.dumps(result, ensure_ascii=True, separators=(",", ":")).encode("utf-8")
        if len(encoded) > OUTPUT_LIMIT:
            raise ValueError("output exceeds limit")
        with output.open("xb") as destination:
            destination.write(encoded)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kotlinc", type=pathlib.Path, required=True)
    parser.add_argument("--plugin", type=pathlib.Path, required=True)
    parser.add_argument("--source", type=pathlib.Path, action="append", required=True)
    parser.add_argument("--jar", type=pathlib.Path, action="append", required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--jvm-target", default="1.8")
    parser.add_argument("--require-class", action="append", default=[])
    args = parser.parse_args()
    run(args.kotlinc, args.plugin, args.source, args.jar, args.output, args.jvm_target, args.require_class)


if __name__ == "__main__":
    main()
