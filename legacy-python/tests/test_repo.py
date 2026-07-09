"""Git-backed version listing (ordered by commit date, no hardcoded list) and
URL/path normalization against a dynamic release set."""

from __future__ import annotations

import subprocess
import types

from sndoc.core import repo

# Real `git for-each-ref --format=%(refname:short) refs/remotes/origin` output:
# branches show as "origin/<name>", the symbolic origin/HEAD alias as bare
# "origin", and non-release branches like "main" are present. Commit-date order
# here ranks xanadu first even though australia (the default branch) is latest.
FOR_EACH_REF = """origin/xanadu
origin/yokohama
origin/zurich
origin/main
origin
origin/australia
"""


def _fake_git(monkeypatch, *, for_each_ref: str, head: str = ""):
    """Dispatch by git subcommand so symbolic-ref and for-each-ref differ."""

    def fake(*args, **kwargs):
        if args and args[0] == "symbolic-ref":
            return types.SimpleNamespace(stdout=head, returncode=0)
        return types.SimpleNamespace(stdout=for_each_ref, returncode=0)

    monkeypatch.setattr(repo, "_git", fake)


def test_branches_by_date_filters_alias_and_non_release(monkeypatch):
    _fake_git(monkeypatch, for_each_ref=FOR_EACH_REF)
    # "origin" (HEAD alias) and "main" dropped; release branches kept in date order.
    assert repo._branches_by_date() == ["xanadu", "yokohama", "zurich", "australia"]


def test_default_branch_is_authoritative_latest(monkeypatch):
    _fake_git(
        monkeypatch,
        for_each_ref=FOR_EACH_REF,
        head="refs/remotes/origin/australia\n",
    )
    assert repo.default_branch() == "australia"
    assert repo.resolve_latest_branch() == "australia"


def test_list_versions_pins_default_branch_first_as_latest(monkeypatch):
    _fake_git(
        monkeypatch,
        for_each_ref=FOR_EACH_REF,
        head="refs/remotes/origin/australia\n",
    )
    versions = repo.list_versions()
    assert versions[0].release == "australia"
    assert versions[0].is_latest is True
    assert all(not v.is_latest for v in versions[1:])
    # The rest follow, newest-first by commit date.
    assert [v.release for v in versions[1:]] == ["xanadu", "yokohama", "zurich"]


def test_resolve_latest_falls_back_to_commit_date_without_head(monkeypatch):
    _fake_git(monkeypatch, for_each_ref=FOR_EACH_REF, head="")
    assert repo.resolve_latest_branch() == "xanadu"  # newest by date


def test_resolve_latest_falls_back_when_no_branches(monkeypatch):
    _fake_git(monkeypatch, for_each_ref="", head="")
    assert repo.resolve_latest_branch() == repo.DEFAULT_BRANCH


RELEASES = {"zurich", "yokohama", "xanadu"}


def test_to_repo_path_full_docs_url():
    rp = repo.to_repo_path(
        "https://www.servicenow.com/docs/bundle/zurich/page/api/glide-record.html",
        RELEASES,
    )
    # NOTE: only a leading release segment is stripped; this exercises .html + host.
    assert rp.repo_path.endswith(".md")


def test_to_repo_path_reader_path_with_release():
    rp = repo.to_repo_path("r/zurich/api/glide-record", RELEASES)
    assert rp.release_in_path == "zurich"
    assert rp.repo_path == "api/glide-record.md"


def test_to_repo_path_plain_repo_path_no_release():
    rp = repo.to_repo_path("api/glide-record.md", RELEASES)
    assert rp.release_in_path is None
    assert rp.repo_path == "api/glide-record.md"


def test_to_repo_path_unknown_leading_segment_is_not_a_release():
    rp = repo.to_repo_path("api/glide-record", RELEASES)
    assert rp.release_in_path is None
    assert rp.repo_path == "api/glide-record.md"


def test_docs_url_for_path():
    assert repo.docs_url_for_path("api/glide-record.md").endswith("/r/api/glide-record")


# --- read_doc_from_clone (git show origin/<branch>:<path>) -----------------

def test_read_doc_from_clone_returns_markdown(monkeypatch):
    captured = {}

    def fake(*args, **kwargs):
        captured["args"] = args
        captured["check"] = kwargs.get("check")
        return types.SimpleNamespace(stdout="# GlideRecord\n\nbody\n", returncode=0)

    monkeypatch.setattr(repo, "_git", fake)
    doc = repo.read_doc_from_clone("api/glide-record.md", "zurich")
    assert doc is not None
    assert doc.markdown == "# GlideRecord\n\nbody\n"
    # Reads the committed blob at the branch tip without checking out / churning.
    assert captured["args"] == ("show", "origin/zurich:markdown/api/glide-record.md")
    assert captured["check"] is False
    assert doc.raw_url.endswith("/zurich/markdown/api/glide-record.md")


def test_read_doc_from_clone_strips_existing_markdown_prefix(monkeypatch):
    captured = {}

    def fake(*args, **kwargs):
        captured["args"] = args
        return types.SimpleNamespace(stdout="x\n", returncode=0)

    monkeypatch.setattr(repo, "_git", fake)
    repo.read_doc_from_clone("markdown/api/glide-record.md", "zurich")
    # No doubled prefix.
    assert captured["args"] == ("show", "origin/zurich:markdown/api/glide-record.md")


def test_read_doc_from_clone_miss_returns_none(monkeypatch):
    def fake(*args, **kwargs):
        return types.SimpleNamespace(stdout="", returncode=128)  # unknown path/branch

    monkeypatch.setattr(repo, "_git", fake)
    assert repo.read_doc_from_clone("nope/missing.md", "zurich") is None


def test_read_doc_from_clone_empty_file_returns_none(monkeypatch):
    def fake(*args, **kwargs):
        return types.SimpleNamespace(stdout="   \n", returncode=0)

    monkeypatch.setattr(repo, "_git", fake)
    assert repo.read_doc_from_clone("api/empty.md", "zurich") is None


def test_read_doc_from_clone_timeout_returns_none(monkeypatch):
    """A wedged git read is a miss, not a hang — the root cause of the MCP fetch
    timeouts on Windows."""

    def fake(*args, **kwargs):
        raise subprocess.TimeoutExpired(cmd="git show", timeout=repo.GIT_TIMEOUT_S)

    monkeypatch.setattr(repo, "_git", fake)
    assert repo.read_doc_from_clone("api/glide-record.md", "zurich") is None


# --- _git hardening (headless-safe git for the MCP stdio server) -----------

def test_git_invocation_is_hardened(monkeypatch):
    captured = {}

    def fake_run(argv, **kwargs):
        captured["argv"] = argv
        captured["kwargs"] = kwargs
        return types.SimpleNamespace(stdout="", stderr="", returncode=0)

    monkeypatch.setattr(repo.subprocess, "run", fake_run)
    repo._git("rev-parse", "HEAD")

    kwargs = captured["kwargs"]
    # Never block on the inherited MCP stdin; always bound the call.
    assert kwargs["stdin"] is subprocess.DEVNULL
    assert isinstance(kwargs["timeout"], (int, float)) and kwargs["timeout"] > 0
    # Never prompt for credentials (no console under an MCP server).
    assert kwargs["env"]["GIT_TERMINAL_PROMPT"] == "0"
    # No fsmonitor daemon to inherit/hold the captured pipes.
    assert "core.fsmonitor=false" in captured["argv"]
    # Decode UTF-8 explicitly, not the platform locale (cp1252 on Windows chokes
    # on non-Latin-1 bytes in a doc and raises UnicodeDecodeError).
    assert kwargs["encoding"] == "utf-8"
    assert kwargs["errors"] == "replace"


def test_git_decodes_utf8_output(monkeypatch):
    """Real subprocess.run with encoding='utf-8' must round-trip a doc byte
    (0x9d inside a UTF-8 sequence) that cp1252 cannot decode."""
    snippet = "GlideRecord — café “quotes”"  # em dash + curly quotes

    def fake_run(argv, **kwargs):
        raw = snippet.encode("utf-8")
        decoded = raw.decode(kwargs["encoding"], kwargs["errors"])
        return types.SimpleNamespace(stdout=decoded, stderr="", returncode=0)

    monkeypatch.setattr(repo.subprocess, "run", fake_run)
    out = repo._git("show", "origin/zurich:markdown/x.md")
    assert out.stdout == snippet
