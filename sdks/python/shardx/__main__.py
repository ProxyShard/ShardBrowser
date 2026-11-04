"""`python -m shardx install`: the same as the `shardx` command."""
import sys

from .cli import main

sys.exit(main())
