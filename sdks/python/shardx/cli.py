from . import ShardX
import argparse
import sys


def install_binary():
    sdk = ShardX()
    sdk.runtime.install()

def main():
    parser = argparse.ArgumentParser(prog="shardx", description="A free, open-source anti-detect browser launcher for web scraping and multi-accounting.")
    subparsers = parser.add_subparsers(dest="command")
    subparsers.add_parser(
        name="install",
        help="Installs the latest ShardBrowser."
    )
    
    args = parser.parse_args()
    
    if args.command == "install":
        install_binary()
    else:
        parser.print_help()
        sys.exit(1 if args.command else 0)

if __name__ == "__main__":
    main()
