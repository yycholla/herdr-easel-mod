#!/usr/bin/env python3
"""Render an artifact through MCP in a clean Herdr sandbox; never drive the real display."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
APP = Path(os.environ['HERDR_ORIGIN_WORKTREE'])
BIN = os.environ['HERDR_ORIGIN_BIN']
NAME = f'easel-mod-{os.getpid()}'
HOME = Path('/var/tmp') / f'herdr-app-{NAME}' / 'home'
ENV = dict(os.environ, HERDR_APP_SANDBOX=NAME, HERDR_APP_BIN=BIN, HERDR_APP_HOME=str(HOME))

def sandbox(*args, check=True):
    result = subprocess.run([str(APP/'scripts/sandbox'), *args], env=ENV,
                            capture_output=True, text=True, timeout=120)
    if check and result.returncode:
        raise AssertionError(f'{args}: {result.stdout}\n{result.stderr}')
    return result.stdout

def command(*args):
    return json.loads(subprocess.check_output([BIN, *args], env=ENV, text=True, timeout=15))

with tempfile.TemporaryDirectory(prefix='easel-mod-ui-project-') as temporary:
    project = Path(temporary)
    HOME.mkdir(parents=True)
    (HOME/'shared.json').write_text('{"frame.welcomed":"skipped"}')
    try:
        sandbox('up')
        sandbox('do', 'space.new', json.dumps({'name':'Easel mod test', 'folder':temporary}))
        world = command('ls')
        terminal = next(s for s in world['spaces'] if s['machine'] is None)['tabs'][0]['terminals'][0]['terminal']
        env = dict(ENV, HC_TERMINAL_ID=terminal, GPUI_ARTIFACTS_PROJECT=temporary,
                   GPUI_ARTIFACTS_HERDR=BIN,
                   GPUI_ARTIFACTS_MOD_HELPER=str(ROOT/'target/debug/gpui-artifacts-mod'))
        env.pop('GPUI_ARTIFACTS_VIEWER', None)
        env.pop('GPUI_ARTIFACTS_SOCKET', None)
        message = {'jsonrpc':'2.0','id':1,'method':'tools/call','params':{
            'name':'render_gpui','arguments':{
                'id':'card','title':'Easel inside stock Herdr',
                'source':(ROOT/'fixtures/card.rs').read_text(), 'mode':'interpret'}}}
        result = subprocess.run([str(ROOT/'target/debug/gpui-artifacts-mcp')],
                                input=json.dumps(message)+'\n', capture_output=True,
                                text=True, env=env, check=True, timeout=20)
        reply = json.loads(result.stdout)
        assert reply['result']['isError'] is False, reply
        time.sleep(.8)
        status = command('status')
        assert status['errors'] == [], status
        action = subprocess.check_output([str(ROOT/'target/debug/gpui-artifacts-mod'),
                    'call', temporary, '{"action":"ping"}'], env=env, text=True)
        assert json.loads(action)['attached'] is True, action
        output = Path(os.environ.get('EASEL_UI_ARTIFACT_DIR', ROOT/'target/herdr-mod-proof'))
        output.mkdir(parents=True, exist_ok=True)
        sandbox('shot', str(output/'stock-herdr-easel.png'))
        (output/'status.json').write_text(json.dumps({'window':status, 'mcp':reply}, indent=2)+'\n')
        assert subprocess.check_output(['git','-C',str(APP),'status','--porcelain'],text=True)==''
        print(f'PASS: MCP opened Easel beside its caller on unmodified Herdr; artifact rendered; no app errors. Screenshot: {output}/stock-herdr-easel.png')
    finally:
        sandbox('down', check=False)
