"""Build the declared RediCloud classpaths for the trusted pinned replay."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from xml.etree import ElementTree as XML

VERSION = "1.8.1"
GROUP = "dev.redicloud.libloader"
RELEASE = f"https://github.com/RediCloud/gradle-plugins/releases/download/v{VERSION}"
ARTIFACTS = {
    "libloader": (
        "54cfdf68c8532ad0cf53d248ee8c4e97f31a21a95909bef1d6da2eeb9f24152d",
        [("org.jetbrains.kotlin", "kotlin-stdlib", "2.3.20")],
    ),
    "libloader-bootstrap": (
        "0263369396ac3f8f1f9c42e65a4a2344c5c295f9e896bbfcbe118b98e631f48d",
        [("com.google.code.gson", "gson", "2.14.0"), ("org.jsoup", "jsoup", "1.22.2")],
    ),
}


def write_pom(path, artifact, dependencies):
    project = XML.Element("project", xmlns="http://maven.apache.org/POM/4.0.0")
    for key, value in (("modelVersion", "4.0.0"), ("groupId", GROUP), ("artifactId", artifact), ("version", VERSION)):
        XML.SubElement(project, key).text = value
    deps = XML.SubElement(project, "dependencies")
    for group, dependency, version in dependencies:
        item = XML.SubElement(deps, "dependency")
        for key, value in (("groupId", group), ("artifactId", dependency), ("version", version), ("scope", "runtime")):
            XML.SubElement(item, key).text = value
    XML.ElementTree(project).write(path, encoding="utf-8", xml_declaration=True)


def prepare_maven(maven):
    # The public release jars are checksum-pinned. The replay reconstructs
    # the corresponding metadata because GitHub Packages requires credentials.
    for artifact, (digest, dependencies) in ARTIFACTS.items():
        destination = maven.joinpath(*GROUP.split("."), artifact, VERSION)
        destination.mkdir(parents=True)
        name = f"{artifact}-{VERSION}"
        jar = destination / f"{name}.jar"
        subprocess.run(["curl", "-fsSL", "--retry", "3", "--proto", "=https",
                        "--output", jar, f"{RELEASE}/{name}.jar"], check=True, timeout=120)
        if hashlib.sha256(jar.read_bytes()).hexdigest() != digest:
            raise ValueError(f"RediCloud artifact digest mismatch: {name}")
        write_pom(destination / f"{name}.pom", artifact, dependencies)


def classpaths(repo, work, maven):
    # The upstream build gets an isolated Gradle home and no runner credentials.
    home = work / "home"
    home.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "JAVA_HOME", "LANG") if key in os.environ}
    env.update(HOME=str(home), GRADLE_USER_HOME=str(work / "gradle-home"),
               REPLAY_MAVEN_URI=maven.as_uri())
    result = subprocess.run(
        [os.environ["REPLAY_GRADLE"], "--no-daemon", "--max-workers=2", "--console=plain",
         "-I", Path(__file__).with_name("redicloud.init.gradle"),
         "-Pkotlin.compiler.execution.strategy=in-process", ":file-cluster:replayClasspaths"],
        cwd=repo, env=env, capture_output=True, text=True, encoding="utf-8", timeout=900,
    )
    for stream, contents in (("stdout", result.stdout), ("stderr", result.stderr)):
        (work / f"gradle.{stream}").write_text(contents, encoding="utf-8")
    result.check_returncode()
    return result.stdout.splitlines()


def jars(lines, prefix):
    files = [Path(line.removeprefix(prefix)).resolve(strict=True)
             for line in lines if line.startswith(prefix)]
    if not files or any(file.suffix != ".jar" or not file.is_file() for file in files):
        raise ValueError(f"invalid RediCloud {prefix.strip()} classpath")
    return list(map(str, files))


def prepare_redicloud(root, repo):
    work = repo / ".replay-kotlin"
    work.mkdir()
    maven = work / "maven"
    prepare_maven(maven)
    tool = Path(os.environ["REPLAY_KOTLIN"])
    plugin = work / "evidence.jar"
    subprocess.run(
        [sys.executable, root / "tools/kotlin-callback-evidence/build.py",
         "--kotlinc", tool / "bin/kotlinc", "--compiler-jar", tool / "lib/kotlin-compiler.jar",
         "--compiler-version", "2.3.21", "--output", plugin],
        check=True, timeout=180,
    )
    lines = classpaths(repo, work, maven)
    sources = "file-cluster/src/main"
    config = {
        "python": sys.executable,
        "kotlinc": str(tool / "bin/kotlinc"),
        "plugin": str(plugin),
        "jvmTarget": "21",
        "sources": sorted(file.relative_to(repo).as_posix() for file in (repo / sources).rglob("*.kt")),
        "sourceRoots": [sources],
        "jars": jars(lines, "COMPILE_JAR "),
        "runtimeJars": jars(lines, "RUNTIME_JAR "),
        "requiredClasses": [
            "org/apache/sshd/server/SshServer.class",
            "org/apache/sshd/server/ServerBuilder.class",
            "org/apache/sshd/common/BaseBuilder.class",
            "org/apache/sshd/server/auth/password/PasswordAuthenticator.class",
        ],
        "trustDeclaredBuildInputs": True,
    }
    path = work / "config.json"
    path.write_text(json.dumps(config), encoding="utf-8")
    return path
