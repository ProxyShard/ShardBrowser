"""`shardx` command line: fetch the engine ahead of the first launch (#132).

    shardx install [--force] [--cache-dir DIR]

Useful in a Docker image, where the first launch should not be the one that
downloads several hundred megabytes.
"""
import argparse
import sys
from typing import Optional

from .runtime import Runtime


def _progress_printer():
    shown: dict[str, int] = {}

    def show(label: str, received: int, total: int) -> None:
        if total <= 0:
            return
        pct = received * 100 // total
        # Every tenth percent, not every chunk: a build log stays readable.
        if pct // 10 != shown.get(label, -1):
            shown[label] = pct // 10
            print(f"{label}: {pct}%", file=sys.stderr, flush=True)

    return show


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(prog="shardx", description="ShardX browser SDK.")
    sub = parser.add_subparsers(dest="command", required=True)
    install = sub.add_parser(
        "install",
        help="download the browser engine, the protected-video module and the "
        "fingerprint library, or update them",
    )
    install.add_argument("--force", action="store_true", help="download again even when up to date")
    install.add_argument("--cache-dir", help="where the engine is kept (default: the SDK's own cache folder)")
    args = parser.parse_args(argv)

    if args.command == "install":
        runtime = Runtime(cache_dir=args.cache_dir, progress=_progress_printer())
        runtime.install(force=args.force)
        print(f"engine {runtime.chromium_version} at {runtime.binary_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
