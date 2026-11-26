#!/usr/bin/env python3
"""Run independent Autobahn 1-7 and 9 verdicts against the wsock echo endpoint."""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import subprocess

from wire_process import start_listening, stop_process
from wire_wsock import wait_ready

IMAGE = 'crossbario/autobahn-testsuite@sha256:519915fb568b04c9383f70a1c405ae3ff44ab9e35835b085239c258b6fac3074'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--echo', type=Path, required=True)
    parser.add_argument('--reports', type=Path, required=True)
    parser.add_argument('--cases', action='append', help='named suite cases; default all required sections')
    parser.add_argument('--resume', action='store_true', help='retain completed sections in this report directory')
    args = parser.parse_args()
    reports = args.reports.resolve()
    reports.mkdir(parents=True, exist_ok=True)
    # Bound each independent invocation by splitting the large UTF-8 section.
    groups = [args.cases] if args.cases else [
        ['1.*', '2.*', '3.*', '4.*', '5.*'],
        [f'6.{i}.*' for i in range(1, 12)],
        [f'6.{i}.*' for i in range(12, 24)],
        ['7.*'], ['9.*'],
    ]
    summary = []
    with (reports/'endpoint.stderr').open('wb') as errors:
        endpoint, port = start_listening(args.echo, stderr=errors)
        try:
            wait_ready(endpoint, port)
            for index, cases in enumerate(groups):
                group = reports/str(index)
                group.mkdir(exist_ok=True)
                (group/'config.json').write_text(json.dumps({
                    'outdir':'/reports/results',
                    'servers':[{'url':f'ws://127.0.0.1:{port}/hirsel', 'agent':'Hirsel-W2'}],
                    'cases':cases, 'exclude-cases':[], 'exclude-agent-cases':{},
                }))
                completed = (group/'results/index.json').is_file()
                if args.resume and completed:
                    print(f'RETAIN {cases}', flush=True)
                else:
                    print(f'START {cases}', flush=True)
                    name = f'wire-w2-1089-autobahn-{os.getpid()}-{index}'
                    try:
                        with (group/'suite.log').open('wb') as log:
                            done = subprocess.run([
                                'docker', 'run', '--rm', '--name', name, '--network', 'host',
                                '--cpuset-cpus', '0-1', '--memory', '2g', '--user', f'{os.getuid()}:{os.getgid()}',
                                '-v', f'{group}:/reports', IMAGE,
                                'wstest', '-m', 'fuzzingclient', '-s', '/reports/config.json',
                            ], stdout=log, stderr=subprocess.STDOUT)
                        assert done.returncode == 0, (cases, done.returncode, str(group/'suite.log'))
                    finally:
                        # Docker clients can leave a container alive after interruption.
                        running = subprocess.run(['docker', 'container', 'inspect', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                        if running.returncode == 0:
                            subprocess.run(['docker', 'stop', name], check=True, stdout=subprocess.DEVNULL)
                found = []
                for file in sorted((group/'results').glob('*.json')):
                    result = json.loads(file.read_text())
                    if isinstance(result, dict) and 'behavior' in result:
                        row = {key: result.get(key) for key in [
                            'id', 'description', 'behavior', 'behaviorClose', 'result',
                            'expected', 'received', 'expectedClose', 'receivedClose',
                            'remoteCloseCode', 'wasClean', 'duration',
                        ]}
                        row['report'] = str(file.relative_to(reports))
                        found.append(row)
                assert found, f'zero suite verdicts for {cases}'
                summary.extend(found)
                (reports/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
                print(f'FINISH {cases}: {len(found)} cases, {dict(Counter(r["behavior"] for r in found))}', flush=True)
                # Keep independent JSON verdicts; large rendered wire logs add no evidence.
                for file in (group/'results').glob('*.html'):
                    file.unlink()
        finally:
            stop_process(endpoint)
    print(f'TOTAL {len(summary)} cases, {dict(Counter(r["behavior"] for r in summary))}, close {dict(Counter(r["behaviorClose"] for r in summary))}', flush=True)
    return int(any(r['behavior'] not in ('OK', 'NON-STRICT', 'INFORMATIONAL') or r['behaviorClose'] not in ('OK', 'NON-STRICT', 'INFORMATIONAL') for r in summary))


if __name__ == '__main__':
    raise SystemExit(main())
