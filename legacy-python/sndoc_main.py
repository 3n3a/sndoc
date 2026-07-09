"""Entry point for the Nuitka onefile binary.

The packaged `sndoc` binary runs the same Typer app exposed by the
`sndoc` console script (`sndoc.cli:app`)."""

from sndoc.cli import app

if __name__ == "__main__":
    app()
