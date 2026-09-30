#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def api(url, token, method="GET", body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = Request(
        url,
        data=data,
        method=method,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
            **({"Content-Type": "application/json"} if data else {}),
        },
    )
    with urlopen(request) as response:
        return json.load(response)


def main():
    repo = os.environ["GITHUB_REPOSITORY"]
    pr = os.environ["PR_NUMBER"]
    token = os.environ["GH_TOKEN"]
    body = Path(sys.argv[1]).read_text()
    comments = api(f"https://api.github.com/repos/{repo}/issues/{pr}/comments?per_page=100", token)
    existing = next((c for c in reversed(comments) if "<!-- coverage-diff -->" in c["body"]), None)
    url = (
        f"https://api.github.com/repos/{repo}/issues/comments/{existing['id']}"
        if existing
        else f"https://api.github.com/repos/{repo}/issues/{pr}/comments"
    )
    try:
        api(url, token, "PATCH" if existing else "POST", {"body": body})
    except HTTPError as error:
        print(f"failed to publish coverage comment: HTTP {error.code}", file=sys.stderr)
        raise


if __name__ == "__main__":
    main()
