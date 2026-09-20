#!/usr/bin/env python3
"""Offline validation of an ACP registry agent.json against the real
schema rules (fetched 2026-09-21 from agentclientprotocol/registry's
agent.schema.json) -- no network access or third-party dependency
needed, since this environment has neither `jsonschema` nor `pip`
available. Re-implements the rules relevant to a binary-distribution
submission with no preview channel; does not attempt to validate every
possible field the full schema supports (e.g. npx/uvx distribution,
preview channels) since this project's submission never uses them.

Usage: python3 docs/registry-submission/validate_agent_json.py \
    docs/registry-submission/aivyx-coder/agent.json
"""
import json
import re
import sys


def fail(msg: str) -> None:
    print(f"FAIL: {msg}")
    sys.exit(1)


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: validate_agent_json.py <path/to/agent.json>")

    with open(sys.argv[1]) as f:
        data = json.load(f)

    for field in ("id", "name", "version", "description", "distribution"):
        if field not in data:
            fail(f"missing required field: {field}")

    if not re.fullmatch(r"[a-z][a-z0-9-]*", data["id"]):
        fail(f"id {data['id']!r} does not match ^[a-z][a-z0-9-]*$")

    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", data["version"]):
        fail(f"version {data['version']!r} does not match ^[0-9]+\\.[0-9]+\\.[0-9]+$")

    if data["id"] != "dimcode" and "license_url" not in data:
        fail("license_url is required (id is not the 'dimcode' exception)")

    if "icon" in data:
        fail(
            "icon field must be omitted -- set automatically by the "
            "registry's own build from the sibling icon.svg file, not "
            "hand-written"
        )

    dist = data["distribution"]
    if not isinstance(dist, dict) or len(dist) < 1:
        fail("distribution must be an object with at least one property")

    allowed_dist_keys = {"binary", "npx", "uvx"}
    extra_dist_keys = set(dist.keys()) - allowed_dist_keys
    if extra_dist_keys:
        fail(f"distribution has disallowed keys: {extra_dist_keys}")

    if "binary" in dist:
        binary = dist["binary"]
        if not isinstance(binary, dict) or len(binary) < 1:
            fail("distribution.binary must be an object with at least one property")
        allowed_platforms = {
            "darwin-aarch64",
            "darwin-x86_64",
            "linux-aarch64",
            "linux-x86_64",
            "windows-aarch64",
            "windows-x86_64",
        }
        for platform, target in binary.items():
            if platform not in allowed_platforms:
                fail(f"distribution.binary key {platform!r} is not an allowed platform")
            for field in ("archive", "cmd"):
                if field not in target:
                    fail(f"distribution.binary.{platform} missing required field: {field}")
            allowed_target_keys = {"archive", "sha256", "cmd", "args", "env"}
            extra_target_keys = set(target.keys()) - allowed_target_keys
            if extra_target_keys:
                fail(f"distribution.binary.{platform} has disallowed keys: {extra_target_keys}")
            if "sha256" in target and not re.fullmatch(r"[a-fA-F0-9]{64}", target["sha256"]):
                fail(f"distribution.binary.{platform}.sha256 is not 64 hex characters")
            if "/latest/" in target["archive"]:
                fail(f"distribution.binary.{platform}.archive must not contain '/latest/'")

    print("PASS: agent.json matches the real registry schema rules")


if __name__ == "__main__":
    main()
