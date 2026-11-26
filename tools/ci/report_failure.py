#!/usr/bin/env python3
"""File or update one open issue per CI check. Serialize callers with Actions concurrency."""

import argparse
import json
import os
import re
import subprocess


def api(*args):
    output = subprocess.check_output(['gh', 'api', *args], text=True)
    return json.loads(output)


def report(repository, key, occurrence):
    issues = f'repos/{repository}/issues'
    marker = f'<!-- hirsel-ci-check: {key} -->'
    pages = api('--paginate', '--slurp', f'{issues}?state=open&per_page=100')
    for page in pages:
        for issue in page:
            if (issue['user']['login'] == 'github-actions[bot]'
                    and 'pull_request' not in issue
                    and marker in (issue['body'] or '').splitlines()):
                comment = api(f'{issues}/{issue["number"]}/comments', '-f', f'body={occurrence}')
                return comment['html_url']
    issue = api(issues, '-f', f'title=CI failure: {key}', '-f', f'body={marker}\n\n{occurrence}')
    return issue['html_url']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('key', help='stable check name')
    parser.add_argument('detail', help='short description of what failed')
    args = parser.parse_args()
    if not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_.:/-]{0,159}', args.key):
        parser.error('invalid check name')
    repository = os.environ['GITHUB_REPOSITORY']
    url = f'{os.environ["GITHUB_SERVER_URL"]}/{repository}/actions/runs/{os.environ["GITHUB_RUN_ID"]}'
    url += f'/attempts/{os.environ["GITHUB_RUN_ATTEMPT"]}'
    occurrence = f'{args.detail}\n\nCommit `{os.environ["GITHUB_SHA"]}`. [Run logs]({url}).'
    print(report(repository, args.key, occurrence))


if __name__ == '__main__':
    main()
