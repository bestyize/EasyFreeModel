"""Compare the checked-in provider catalog with a local 9Router registry.

Reads registry/index.js as text; it never imports or executes reference code.
"""

import argparse
import json
import re
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
REFERENCE_COMMIT = "87a74e1bda3677193c6f71a7e2100c90311f0689"
IMPORT = re.compile(r'^import\s+(p\w+)\s+from\s+"\./([^"/]+)\.js";')
ACTIVE = re.compile(r'^\s+(p\w+),\s*$')


def active_reference_ids(index: Path) -> set[str]:
    lines = index.read_text(encoding="utf-8").splitlines()
    imports = {m.group(1): m.group(2) for line in lines if (m := IMPORT.match(line))}
    names = {m.group(1) for line in lines if (m := ACTIVE.match(line))}
    unknown = names - imports.keys()
    if unknown:
        raise ValueError(f"registry index has unresolved imports: {sorted(unknown)}")
    return {imports[name] for name in names}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference", type=Path, default=ROOT / ".tmp" / "9router")
    args = parser.parse_args()
    commit = subprocess.run(
        ["git", "-C", str(args.reference), "rev-parse", "HEAD"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    if commit != REFERENCE_COMMIT:
        raise SystemExit(f"Expected 9Router {REFERENCE_COMMIT}, found {commit}")
    reference = active_reference_ids(args.reference / "open-sse" / "providers" / "registry" / "index.js")
    catalog = json.loads((ROOT / "src-tauri" / "resources" / "provider_catalog.json").read_text(encoding="utf-8"))
    status = json.loads((ROOT / "src-tauri" / "resources" / "provider_status.json").read_text(encoding="utf-8"))
    oauth = json.loads((ROOT / "src-tauri" / "resources" / "oauth_providers.json").read_text(encoding="utf-8"))["providers"]
    current = {item["id"] for item in catalog}
    pending_chat = sorted(item["id"] for item in catalog
                          if "llm" in (item.get("serviceKinds") or ["llm"])
                          and not status[item["id"]].get("adapter"))
    print(json.dumps({
        "referenceCommit": REFERENCE_COMMIT,
        "referenceActive": len(reference),
        "currentCatalog": len(current),
        "missing": sorted(reference - current),
        "extra": sorted(current - reference),
        "chatAdapterReady": sum(bool(status[item["id"]].get("adapter"))
                                for item in catalog if "llm" in (item.get("serviceKinds") or ["llm"])),
        "pendingChatAdapters": pending_chat,
        "customOAuthFlows": sorted(item["id"] for item in oauth if item["flow"] == "custom"),
    }, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
